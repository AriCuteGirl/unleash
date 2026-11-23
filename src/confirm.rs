//! A yes/no question in the rice's look (GTK's AlertDialog brings its own title bar and colours).

use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// Ask; true when the user picked `yes`. `danger` paints the yes button red.
pub async fn ask(parent: &gtk::Window, title: &str, detail: &str, yes: &str, danger: bool) -> bool {
    let dlg = gtk::Window::builder().transient_for(parent).modal(true).title(title).decorated(false).default_width(400).resizable(false).build();
    dlg.add_css_class("unleash");
    dlg.add_css_class("dialog");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("dialog-root");
    let t = gtk::Label::new(Some(title));
    t.add_css_class("dialog-title");
    t.set_xalign(0.0);
    t.set_wrap(true);
    t.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    root.append(&t);
    if !detail.is_empty() {
        let d = gtk::Label::new(Some(detail));
        d.add_css_class("dialog-hint");
        d.set_xalign(0.0);
        d.set_wrap(true);
        d.set_wrap_mode(gtk::pango::WrapMode::WordChar);
        d.set_max_width_chars(50);
        root.append(&d);
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    row.set_margin_top(6);
    let no = gtk::Button::with_label("Cancel");
    no.add_css_class("secondary");
    let ok = gtk::Button::with_label(yes);
    ok.add_css_class(if danger { "danger" } else { "primary" });
    row.append(&no);
    row.append(&ok);
    root.append(&row);
    dlg.set_child(Some(&root));
    // a destructive question starts on Cancel, so a stray Enter doesn't delete anything
    dlg.set_default_widget(Some(if danger { &no } else { &ok }));

    // 0 = open, 1 = no, 2 = yes
    let answer = Rc::new(Cell::new(0u8));
    for (b, v) in [(&no, 1u8), (&ok, 2u8)] {
        let (a, dw) = (answer.clone(), dlg.downgrade());
        b.connect_clicked(move |_| {
            a.set(v);
            if let Some(d) = dw.upgrade() {
                d.close();
            }
        });
    }
    let a = answer.clone();
    dlg.connect_close_request(move |_| {
        if a.get() == 0 {
            a.set(1);
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
    if danger {
        no.grab_focus();
    } else {
        ok.grab_focus();
    }
    while answer.get() == 0 {
        glib::timeout_future(Duration::from_millis(30)).await;
    }
    answer.get() == 2
}
