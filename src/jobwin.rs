//! A small progress window for `unleash x` / `unleash a` (what Burrow calls). It closes itself a
//! moment after a job succeeds; on an error it stays and says what went wrong.

use crate::backend::{self, Placement, Progress, create};
use crate::looks;
use crate::util::human_size;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub enum Job {
    Extract { to: Option<PathBuf>, archives: Vec<PathBuf> },
    Add { format: create::Format, name: String, to: PathBuf, level: create::Level, files: Vec<PathBuf> },
}

fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

pub fn run(app: &gtk::Application, job: Job) {
    looks::get();
    let title_text = match &job {
        Job::Extract { archives, .. } if archives.len() == 1 => format!("Extracting {}", name_of(&archives[0])),
        Job::Extract { archives, .. } => format!("Extracting {} archives", archives.len()),
        Job::Add { name, format, .. } => format!("Compressing {name}{}", format.ext()),
    };
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title(format!("{title_text} — Unleash"))
        .icon_name(crate::APP_ID)
        .default_width(460)
        .resizable(false)
        .decorated(false)
        .build();
    window.add_css_class("unleash");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("unleash-root");
    root.add_css_class("job-window");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon = gtk::Image::from_gicon(&gio::ThemedIcon::from_names(&[crate::APP_ID, "package-x-generic", "package-x-generic-symbolic"]));
    icon.set_pixel_size(40);
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    texts.set_hexpand(true);
    let title = gtk::Label::new(Some(&title_text));
    title.add_css_class("job-title");
    title.set_xalign(0.0);
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let detail = gtk::Label::new(Some("Starting…"));
    detail.add_css_class("job-detail");
    detail.set_xalign(0.0);
    detail.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    texts.append(&title);
    texts.append(&detail);
    head.append(&icon);
    head.append(&texts);
    let bar = gtk::ProgressBar::new();
    bar.add_css_class("job");
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    buttons.set_halign(gtk::Align::End);
    let show = gtk::Button::with_label("Show");
    show.add_css_class("secondary");
    show.set_visible(false);
    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("secondary");
    buttons.append(&show);
    buttons.append(&cancel);
    root.append(&head);
    root.append(&bar);
    root.append(&buttons);
    window.set_child(Some(&root));
    window.present();

    let progress = Arc::new(Progress::default());
    let finished = Rc::new(Cell::new(false));
    {
        let (p, w, f) = (progress.clone(), window.downgrade(), finished.clone());
        cancel.connect_clicked(move |_| {
            if f.get() {
                if let Some(w) = w.upgrade() {
                    w.close();
                }
            } else {
                p.cancel.store(true, Ordering::Relaxed);
            }
        });
    }
    {
        // closing the window cancels a running job
        let (p, f) = (progress.clone(), finished.clone());
        window.connect_close_request(move |_| {
            if !f.get() {
                p.cancel.store(true, Ordering::Relaxed);
            }
            glib::Propagation::Proceed
        });
    }
    // a step counter shared with the worker: "2 of 3"
    let step = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let steps = match &job {
        Job::Extract { archives, .. } => archives.len(),
        Job::Add { .. } => 1,
    };
    let ticker = {
        let (p, bar, detail, step) = (progress.clone(), bar.clone(), detail.clone(), step.clone());
        glib::timeout_add_local(Duration::from_millis(100), move || {
            bar.set_fraction(p.fraction());
            let cur = p.current.lock().map(|c| c.rsplit('/').next().unwrap_or("").to_string()).unwrap_or_default();
            let (d, t) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
            let mut s = format!("{} of {}", human_size(d.min(t)), human_size(t));
            if steps > 1 {
                s = format!("{} of {steps} · {s}", step.load(Ordering::Relaxed) + 1);
            }
            if !cur.is_empty() {
                s.push_str(&format!(" · {cur}"));
            }
            detail.set_text(&s);
            glib::ControlFlow::Continue
        })
    };

    let w = window.downgrade();
    glib::spawn_future_local(async move {
        let p = progress.clone();
        let r: Result<Vec<PathBuf>, String> = gio::spawn_blocking(move || work(job, &p, &step)).await.unwrap_or_else(|_| Err("It crashed".into()));
        ticker.remove();
        finished.set(true);
        let Some(window) = w.upgrade() else { return };
        cancel.set_label("Close");
        match r {
            Err(e) if e == backend::CANCELLED => window.close(),
            Err(e) => {
                root.add_css_class("error");
                // the whole reason, wrapped (a middle "…" could hide the important part)
                detail.set_ellipsize(gtk::pango::EllipsizeMode::None);
                detail.set_wrap(true);
                detail.set_wrap_mode(gtk::pango::WrapMode::WordChar);
                detail.set_max_width_chars(48);
                detail.set_selectable(true);
                title.set_text(&title.text().replacen("Extracting", "Couldn't extract", 1).replacen("Compressing", "Couldn't compress", 1));
                detail.set_text(&e);
                bar.set_visible(false);
            }
            Ok(made) => {
                root.add_css_class("done");
                bar.set_fraction(1.0);
                title.set_text(&match made.as_slice() {
                    [one] => format!("Done: {}", name_of(one)),
                    v => format!("Done: {} items", v.len()),
                });
                detail.set_text(&made.first().and_then(|m| m.parent()).map(|d| d.display().to_string()).unwrap_or_default());
                show.set_visible(true);
                let first = made.first().cloned();
                let win = window.downgrade();
                show.connect_clicked(move |_| {
                    let (Some(f), Some(win)) = (first.clone(), win.upgrade()) else { return };
                    let l = gtk::FileLauncher::new(Some(&gio::File::for_path(&f)));
                    glib::spawn_future_local(async move {
                        let _ = l.open_containing_folder_future(Some(&win)).await;
                        win.close();
                    });
                });
                // out of the way soon, unless the pointer is on it
                let hover = gtk::EventControllerMotion::new();
                let inside = Rc::new(Cell::new(false));
                let (i1, i2) = (inside.clone(), inside.clone());
                hover.connect_enter(move |_, _, _| i1.set(true));
                hover.connect_leave(move |_| i2.set(false));
                window.add_controller(hover);
                let win = window.downgrade();
                glib::timeout_add_local(Duration::from_millis(2500), move || {
                    let Some(w) = win.upgrade() else { return glib::ControlFlow::Break };
                    if inside.get() {
                        return glib::ControlFlow::Continue;
                    }
                    w.close();
                    glib::ControlFlow::Break
                });
            }
        }
    });
}

fn work(job: Job, p: &Progress, step: &std::sync::atomic::AtomicUsize) -> Result<Vec<PathBuf>, String> {
    match job {
        Job::Extract { to, archives } => {
            let mut made = vec![];
            for (k, a) in archives.iter().enumerate() {
                step.store(k, Ordering::Relaxed);
                p.set_current(&name_of(a));
                let listing = backend::list(a).map_err(|e| format!("{}: {e}", name_of(a)))?;
                let dest = to.clone().or_else(|| a.parent().map(Path::to_path_buf)).ok_or("No folder to extract into")?;
                let r = backend::extract_to(a, &listing, &[], "", &dest, Placement::Smart, p);
                if p.cancelled() {
                    return Err(backend::CANCELLED.into());
                }
                made.extend(r.map_err(|e| if archives.len() == 1 { e } else { format!("{}: {e}", name_of(a)) })?);
            }
            Ok(made)
        }
        Job::Add { format, name, to, level, files } => create::create(&files, &to, &name, format, level, p).map(|a| vec![a]),
    }
}
