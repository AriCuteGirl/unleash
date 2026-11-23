//! "New archive": name, format, level, where to save it. Opens from the New button, or when
//! files (not one archive) are dropped on an empty window.

use crate::backend::create::{self, Format, Level};
use crate::ui::{Win, file_name};
use crate::util::human_size;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

const FORMATS: [(Format, &str); 5] = [
    (Format::Zip, "ZIP · opens everywhere"),
    (Format::SevenZ, "7z · smallest"),
    (Format::TarGz, "tar.gz · Linux standard"),
    (Format::TarXz, "tar.xz · small, slower"),
    (Format::TarZst, "tar.zst · fast and small"),
];
const LEVELS: [(Level, &str); 3] = [(Level::Fast, "Fast"), (Level::Normal, "Normal"), (Level::Best, "Smallest (slow)")];

fn default_name(files: &[PathBuf]) -> String {
    let n = if files.len() == 1 {
        let f = &files[0];
        if f.is_dir() { Some(file_name(f)) } else { f.file_stem().map(|s| s.to_string_lossy().to_string()) }
    } else {
        files[0].parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string())
    };
    n.filter(|s| !s.is_empty()).unwrap_or_else(|| "Archive".into())
}

fn field(label: &str, w: &impl IsA<gtk::Widget>) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    let l = gtk::Label::new(Some(label));
    l.add_css_class("field-label");
    l.set_xalign(0.0);
    b.append(&l);
    b.append(w);
    b
}

pub fn show(win: &Rc<Win>, files: Vec<PathBuf>) {
    let dlg = gtk::Window::builder()
        .transient_for(&win.window)
        .modal(true)
        .title("New archive")
        .decorated(false)
        .default_width(440)
        .resizable(false)
        .build();
    dlg.add_css_class("unleash");
    dlg.add_css_class("dialog");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 14);
    root.add_css_class("dialog-root");

    let title = gtk::Label::new(Some(&if files.len() == 1 { format!("Pack “{}”", file_name(&files[0])) } else { format!("Pack {} items", files.len()) }));
    title.add_css_class("dialog-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let hint = gtk::Label::new(Some("Counting…"));
    hint.add_css_class("dialog-hint");
    hint.set_xalign(0.0);
    let head = gtk::Box::new(gtk::Orientation::Vertical, 2);
    head.append(&title);
    head.append(&hint);
    root.append(&head);

    // name + extension
    let name_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    let entry = gtk::Entry::new();
    entry.add_css_class("path-entry");
    entry.set_hexpand(true);
    entry.set_text(&default_name(&files));
    entry.set_activates_default(true);
    let ext = gtk::Label::new(Some(".zip"));
    ext.add_css_class("ext-suffix");
    name_row.append(&entry);
    name_row.append(&ext);
    root.append(&field("Name", &name_row));

    let fmt = gtk::DropDown::from_strings(&FORMATS.map(|f| f.1));
    let level = gtk::DropDown::from_strings(&LEVELS.map(|l| l.1));
    level.set_selected(1);
    let two = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    two.set_homogeneous(true);
    two.append(&field("Format", &fmt));
    two.append(&field("Compression", &level));
    root.append(&two);

    // where
    let dest = Rc::new(RefCell::new(files[0].parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))));
    let where_btn = gtk::Button::new();
    where_btn.add_css_class("folder-pick");
    let where_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    where_box.append(&gtk::Image::from_icon_name("folder-symbolic"));
    let where_label = gtk::Label::new(Some(&tilde(&dest.borrow())));
    where_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
    where_label.set_xalign(0.0);
    where_label.set_hexpand(true);
    where_box.append(&where_label);
    where_btn.set_child(Some(&where_box));
    root.append(&field("Save in", &where_btn));

    // password (zip and 7z only)
    let pw_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let pw1 = gtk::PasswordEntry::new();
    pw1.set_show_peek_icon(true);
    pw1.add_css_class("path-entry");
    pw1.set_property("placeholder-text", "No password");
    let pw2 = gtk::PasswordEntry::new();
    pw2.set_show_peek_icon(true);
    pw2.add_css_class("path-entry");
    pw2.set_property("placeholder-text", "Repeat it");
    pw2.set_visible(false);
    let pw_hint = gtk::Label::new(Some("zip keeps the file names readable; only their contents are locked (AES-256)"));
    pw_hint.add_css_class("dialog-hint");
    pw_hint.set_xalign(0.0);
    pw_hint.set_wrap(true);
    pw_hint.set_visible(false);
    pw_box.append(&pw1);
    pw_box.append(&pw2);
    pw_box.append(&pw_hint);
    let pw_field = field("Password", &pw_box);
    root.append(&pw_field);
    {
        let (pw2, hint) = (pw2.clone(), pw_hint.clone());
        pw1.connect_changed(move |e| {
            let on = !e.text().is_empty();
            pw2.set_visible(on);
            hint.set_visible(on);
        });
    }

    let open_after = gtk::CheckButton::with_label("Open it when it's done");
    open_after.set_active(true);
    root.append(&open_after);

    let error = gtk::Label::new(None);
    error.add_css_class("form-error");
    error.set_xalign(0.0);
    error.set_wrap(true);
    error.set_visible(false);
    root.append(&error);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("secondary");
    let ok = gtk::Button::with_label("Create");
    ok.add_css_class("primary");
    row.append(&cancel);
    row.append(&ok);
    root.append(&row);
    dlg.set_child(Some(&root));
    dlg.set_default_widget(Some(&ok));

    // size of what's going in, counted off the UI thread
    {
        let (files, hint) = (files.clone(), hint.clone());
        glib::spawn_future_local(async move {
            let r = gio::spawn_blocking(move || create::walk(&files).map(|v| (v.iter().filter(|f| f.2.is_file()).count(), v.iter().filter(|f| f.2.is_file()).map(|f| f.2.len()).sum::<u64>())))
                .await;
            match r {
                Ok(Ok((n, size))) => hint.set_text(&format!("{n} {} · {}", if n == 1 { "file" } else { "files" }, human_size(size))),
                Ok(Err(e)) => hint.set_text(&e),
                Err(_) => hint.set_text(""),
            }
        });
    }
    {
        let (ext, pw_field, hint) = (ext.clone(), pw_field.clone(), pw_hint.clone());
        fmt.connect_selected_notify(move |d| {
            let f = FORMATS[(d.selected() as usize).min(4)].0;
            ext.set_text(f.ext());
            // tar can't be encrypted
            pw_field.set_visible(create::can_encrypt(f));
            hint.set_text(if f == Format::SevenZ {
                "7z locks the file names too: nothing shows without the password"
            } else {
                "zip keeps the file names readable; only their contents are locked (AES-256)"
            });
        });
    }
    {
        let (dw, dest, label) = (dlg.downgrade(), dest.clone(), where_label.clone());
        where_btn.connect_clicked(move |_| {
            let Some(dlg) = dw.upgrade() else { return };
            let d = gtk::FileDialog::builder().title("Save the archive in").modal(true).initial_folder(&gio::File::for_path(&*dest.borrow())).build();
            let (dest, label) = (dest.clone(), label.clone());
            glib::spawn_future_local(async move {
                if let Ok(f) = d.select_folder_future(Some(&dlg)).await {
                    if let Some(p) = f.path() {
                        label.set_text(&tilde(&p));
                        *dest.borrow_mut() = p;
                    }
                }
            });
        });
    }
    {
        let dw = dlg.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = dw.upgrade() {
                d.close();
            }
        });
    }
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

    let (ww, dw) = (Rc::downgrade(win), dlg.downgrade());
    ok.connect_clicked(move |_| {
        let (Some(w), Some(d)) = (ww.upgrade(), dw.upgrade()) else { return };
        let name = entry.text().trim().to_string();
        let bad = if name.is_empty() {
            Some("Give it a name")
        } else if name.contains('/') {
            Some("A name can't contain /")
        } else if name == "." || name == ".." {
            Some("That name isn't allowed")
        } else {
            None
        };
        let format = FORMATS[(fmt.selected() as usize).min(4)].0;
        let pw = Some(pw1.text().to_string()).filter(|p| !p.is_empty() && create::can_encrypt(format));
        let bad = bad.or_else(|| (pw.is_some() && pw1.text() != pw2.text()).then_some("The two passwords aren't the same"));
        if let Some(b) = bad {
            error.set_text(b);
            error.set_visible(true);
            return;
        }
        let lvl = LEVELS[(level.selected() as usize).min(2)].0;
        let to = dest.borrow().clone();
        let files = files.clone();
        let open = open_after.is_active();
        d.close();
        let full = format!("{name}{}", format.ext());
        w.run_job(
            format!("Packing {full}"),
            format!("Couldn't pack {full}"),
            move |p| create::create_pw(&files, &to, &name, format, lvl, p, pw.as_deref()),
            move |w, made| {
                if open {
                    w.job_hide();
                    w.open_archive(made);
                } else {
                    w.job_done(std::slice::from_ref(&made), made.parent().unwrap_or(std::path::Path::new("/")));
                }
            },
        );
    });
    dlg.present();
}

fn tilde(p: &std::path::Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match p.strip_prefix(&home) {
        Ok(r) if !home.as_os_str().is_empty() => {
            if r.as_os_str().is_empty() { "~".into() } else { format!("~/{}", r.display()) }
        }
        _ => p.display().to_string(),
    }
}
