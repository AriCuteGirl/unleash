//! The app's look, shared by every window: one CSS provider for the display, the rice and config
//! watched once, and windows told when something changed (to redraw icons, update menus).

use crate::config::{self, Config};
use crate::theme::{self, RicePaths, Tokens};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

pub struct Looks {
    provider: gtk::CssProvider,
    pub paths: RicePaths,
    pub cfg: RefCell<Config>,
    pub tokens: RefCell<Tokens>,
    icon_theme: RefCell<Option<String>>,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    debounce: RefCell<Option<glib::SourceId>>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
}

thread_local! {
    static LOOKS: RefCell<Option<Rc<Looks>>> = const { RefCell::new(None) };
}

/// The shared look (made on first use, which is at startup: the files are tiny).
pub fn get() -> Rc<Looks> {
    LOOKS.with(|l| {
        if let Some(x) = l.borrow().as_ref() {
            return x.clone();
        }
        let paths = RicePaths::from_env();
        let cfg = config::load();
        let tokens = theme::load_choice(&paths, &cfg.appearance.colors);
        let icon_theme = config::icon_theme(&cfg);
        let provider = gtk::CssProvider::new();
        if let Some(d) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&d, &provider, gtk::STYLE_PROVIDER_PRIORITY_USER + 1);
        }
        let x = Rc::new(Looks {
            provider,
            paths,
            cfg: RefCell::new(cfg),
            tokens: RefCell::new(tokens),
            icon_theme: RefCell::new(icon_theme),
            monitors: RefCell::new(vec![]),
            debounce: RefCell::new(None),
            listeners: RefCell::new(vec![]),
        });
        x.apply();
        x.watch();
        *l.borrow_mut() = Some(x.clone());
        x
    })
}

impl Looks {
    /// Called after every change (the callback should hold only weak references).
    pub fn on_change(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    /// "rice", or the pinned rice id when that rice exists.
    pub fn colors_choice(&self) -> String {
        let c = self.cfg.borrow().appearance.colors.clone();
        if theme::pinned_id(&self.paths, &c).is_some() { c } else { "rice".into() }
    }

    fn apply(&self) {
        if let (Some(t), Some(s)) = (self.icon_theme.borrow().clone(), gtk::Settings::default()) {
            if s.gtk_icon_theme_name().as_deref() != Some(t.as_str()) {
                s.set_gtk_icon_theme_name(Some(&t));
            }
        }
        let cfg = self.cfg.borrow();
        let a = theme::Appearance { font: cfg.appearance.font.clone(), opacity: cfg.appearance.opacity, ..Default::default() };
        self.provider.load_from_string(&theme::css(&self.tokens.borrow(), &a));
    }

    fn watch(self: &Rc<Self>) {
        let mut files = vec![self.paths.current_file()];
        let choice = self.cfg.borrow().appearance.colors.clone();
        if let Some(id) = theme::pinned_id(&self.paths, &choice).or_else(|| theme::active_id(&self.paths)) {
            if let Some(d) = theme::rice_dir(&self.paths, &id) {
                files.push(d.join("rice.json"));
                files.push(d.join("hypr/look.lua"));
            }
        }
        files.push(self.paths.noctalia.clone());
        files.push(config::path());
        let mut mons = vec![];
        for p in files {
            let f = gio::File::for_path(&p);
            let m = if p.is_dir() {
                f.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            } else {
                f.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            };
            let Ok(m) = m else { continue };
            let weak = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, ev| {
                if matches!(ev, gio::FileMonitorEvent::AttributeChanged | gio::FileMonitorEvent::PreUnmount) {
                    return;
                }
                if let Some(x) = weak.upgrade() {
                    x.reload();
                }
            });
            mons.push(m);
        }
        *self.monitors.borrow_mut() = mons;
    }

    /// Re-read rice and config (debounced, on a worker thread) and re-theme if anything changed.
    pub fn reload(self: &Rc<Self>) {
        if let Some(id) = self.debounce.borrow_mut().take() {
            id.remove();
        }
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(250), move || {
            let Some(x) = weak.upgrade() else { return };
            *x.debounce.borrow_mut() = None;
            let paths = x.paths.clone();
            let weak = Rc::downgrade(&x);
            glib::spawn_future_local(async move {
                let Ok((cfg, t, icons)) = gio::spawn_blocking(move || {
                    let cfg = config::load();
                    let t = theme::load_choice(&paths, &cfg.appearance.colors);
                    let icons = config::icon_theme(&cfg);
                    (cfg, t, icons)
                })
                .await
                else {
                    return;
                };
                let Some(x) = weak.upgrade() else { return };
                let rewatch = x.tokens.borrow().rice_id != t.rice_id || x.cfg.borrow().appearance.colors != cfg.appearance.colors;
                let changed = *x.tokens.borrow() != t || *x.cfg.borrow() != cfg || *x.icon_theme.borrow() != icons;
                if changed {
                    *x.tokens.borrow_mut() = t;
                    *x.cfg.borrow_mut() = cfg;
                    *x.icon_theme.borrow_mut() = icons;
                    x.apply();
                    for f in x.listeners.borrow().iter() {
                        f();
                    }
                }
                if rewatch {
                    x.watch();
                }
            });
        });
        *self.debounce.borrow_mut() = Some(id);
    }
}
