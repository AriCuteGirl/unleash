//! The window: toolbar, path bar inside the archive, file list, progress strip, status bar.
//! All reading and extracting happens on worker threads; the UI only polls progress.

use crate::backend::{self, Placement, Progress, modify};
use crate::config;
use crate::looks::{self, Looks};
use crate::drag::{self, LazyFiles, Opened, Pending};
use crate::theme;
use crate::util::{human_size, natural_cmp};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

pub struct Win {
    pub window: gtk::ApplicationWindow,
    looks: Rc<Looks>,

    arc: RefCell<Option<Arc<Opened>>>,
    cwd: Cell<usize>,
    /// extracted copies for drags and "open", by selection, for the open archive
    cache: RefCell<HashMap<Vec<String>, Rc<Pending>>>,
    loading: Cell<u64>,

    stack: gtk::Stack,
    store: gio::ListStore,
    sel: gtk::MultiSelection,
    view: gtk::ColumnView,
    crumbs: gtk::Box,
    status: gtk::Label,
    kind_pill: gtk::Label,
    state_title: gtk::Label,
    state_text: gtk::Label,
    state_icon: gtk::Image,
    state_open: gtk::Button,
    job: JobBar,
    job_busy: Cell<bool>,
    /// files opened in their apps, watched for saves (dropped when another archive opens)
    edit_watch: RefCell<Vec<gio::FileMonitor>>,
    keep_watches: Cell<bool>,
    ctx_menu: gtk::PopoverMenu,
    extract_btns: Vec<gtk::Widget>,
}

struct JobBar {
    revealer: gtk::Revealer,
    root: gtk::Box,
    title: gtk::Label,
    detail: gtk::Label,
    bar: gtk::ProgressBar,
    cancel: gtk::Button,
    open: gtk::Button,
    close: gtk::Button,
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    if !class.is_empty() {
        l.add_css_class(class);
    }
    l
}

fn tool_button(icon: &str, text: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::new();
    let bx = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    bx.append(&gtk::Image::from_icon_name(icon));
    bx.append(&gtk::Label::new(Some(text)));
    b.set_child(Some(&bx));
    b.set_tooltip_text(Some(tip));
    b.add_css_class("tool-btn");
    b
}

pub fn open_window(app: &gtk::Application, file: Option<gio::File>) -> Rc<Win> {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        // an archive's name from the start: window rules can tell archive windows apart
        .title(match file.as_ref().and_then(|f| f.basename()) {
            Some(n) => format!("{} — Unleash", n.to_string_lossy()),
            None => "Unleash".to_string(),
        })
        .icon_name(crate::APP_ID)
        .default_width(980)
        .default_height(640)
        .decorated(false)
        .build();
    window.add_css_class("unleash");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    root.add_css_class("unleash-root");

    // ---- toolbar
    let toolbar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    toolbar.add_css_class("toolbar");
    let name = label("Unleash", "app-name");
    toolbar.append(&name);
    let open = tool_button("document-open-symbolic", "Open", "Open an archive (Ctrl+O)");
    open.set_action_name(Some("win.open"));
    toolbar.append(&open);
    let here = tool_button("archive-extract-symbolic", "Extract here", "Extract next to the archive (Ctrl+Shift+E)");
    here.remove_css_class("tool-btn");
    here.add_css_class("primary");
    here.set_action_name(Some("win.extract-here"));
    let to = tool_button("folder-download-symbolic", "Extract to…", "Pick a folder to extract into (Ctrl+E)");
    to.set_action_name(Some("win.extract-to"));
    let new_btn = tool_button("document-new-symbolic", "New", "Make a new archive (Ctrl+N)");
    new_btn.set_action_name(Some("win.new"));
    toolbar.append(&new_btn);
    let sep = gtk::Separator::new(gtk::Orientation::Vertical);
    sep.add_css_class("tool-sep");
    toolbar.append(&sep);
    toolbar.append(&here);
    toolbar.append(&to);
    let add_btn = tool_button("list-add-symbolic", "Add", "Add files into this folder of the archive (or drop them on the window)");
    add_btn.set_action_name(Some("win.add"));
    let del_btn = tool_button("edit-delete-symbolic", "Delete", "Delete the selected items from the archive (Delete)");
    del_btn.set_action_name(Some("win.delete"));
    toolbar.append(&add_btn);
    toolbar.append(&del_btn);
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    toolbar.append(&spacer);
    let menu_btn = gtk::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Menu"));
    menu_btn.add_css_class("tool-btn");
    toolbar.append(&menu_btn);
    root.append(&toolbar);

    // ---- path bar
    let pathrow = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    pathrow.set_margin_start(12);
    pathrow.set_margin_end(12);
    pathrow.set_margin_top(8);
    pathrow.set_margin_bottom(4);
    let up = gtk::Button::from_icon_name("go-up-symbolic");
    up.add_css_class("flat-btn");
    up.set_tooltip_text(Some("Up (Backspace)"));
    up.set_action_name(Some("win.up"));
    pathrow.append(&up);
    let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 2);
    crumbs.add_css_class("pathbar");
    crumbs.set_hexpand(true);
    pathrow.append(&crumbs);

    // ---- list
    let store = gio::ListStore::new::<glib::BoxedAnyObject>();
    let view = gtk::ColumnView::new(None::<gtk::MultiSelection>);
    view.add_css_class("files");
    view.set_show_column_separators(false);
    view.set_reorderable(false);
    let sorted = gtk::SortListModel::new(Some(store.clone()), view.sorter());
    let sel = gtk::MultiSelection::new(Some(sorted));
    view.set_model(Some(&sel));
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_child(Some(&view));
    scroller.set_vexpand(true);

    let list_page = gtk::Box::new(gtk::Orientation::Vertical, 0);
    list_page.append(&pathrow);
    list_page.append(&scroller);

    // ---- empty / loading / error page
    let state = gtk::Box::new(gtk::Orientation::Vertical, 10);
    state.add_css_class("state-box");
    state.add_css_class("drop-zone");
    state.set_valign(gtk::Align::Fill);
    state.set_vexpand(true);
    let inner = gtk::Box::new(gtk::Orientation::Vertical, 10);
    inner.set_valign(gtk::Align::Center);
    inner.set_vexpand(true);
    let state_icon = gtk::Image::from_icon_name("package-x-generic-symbolic");
    state_icon.set_pixel_size(64);
    state_icon.add_css_class("state-icon");
    let state_title = label("Drop an archive here", "state-title");
    state_title.set_xalign(0.5);
    let state_text = label("zip, 7z, rar, tar.gz, tar.zst, iso and many more", "state-text");
    state_text.set_xalign(0.5);
    state_text.set_wrap(true);
    state_text.set_justify(gtk::Justification::Center);
    let open2 = gtk::Button::with_label("Open an archive…");
    open2.add_css_class("secondary");
    open2.set_halign(gtk::Align::Center);
    open2.set_action_name(Some("win.open"));
    for w in [state_icon.upcast_ref::<gtk::Widget>(), state_title.upcast_ref(), state_text.upcast_ref(), open2.upcast_ref()] {
        inner.append(w);
    }
    state.append(&inner);

    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    stack.add_named(&state, Some("state"));
    stack.add_named(&list_page, Some("list"));
    stack.set_vexpand(true);
    root.append(&stack);

    // ---- job strip
    let job = {
        let rootb = gtk::Box::new(gtk::Orientation::Vertical, 6);
        rootb.add_css_class("jobbar");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
        let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
        texts.set_hexpand(true);
        let title = label("", "job-title");
        let detail = label("", "");
        detail.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        texts.append(&title);
        texts.append(&detail);
        row.append(&texts);
        let open = gtk::Button::with_label("Show");
        open.add_css_class("secondary");
        let cancel = gtk::Button::with_label("Cancel");
        cancel.add_css_class("secondary");
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat-btn");
        close.set_tooltip_text(Some("Dismiss"));
        for b in [&open, &cancel, &close] {
            b.set_valign(gtk::Align::Center);
            row.append(b);
        }
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("job");
        rootb.append(&row);
        rootb.append(&bar);
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        revealer.set_child(Some(&rootb));
        JobBar { revealer, root: rootb, title, detail, bar, cancel, open, close }
    };
    root.append(&job.revealer);

    // ---- status bar
    let statusbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    statusbar.add_css_class("statusbar");
    let status = label("", "");
    status.set_hexpand(true);
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let kind_pill = label("", "pill");
    kind_pill.set_visible(false);
    statusbar.append(&status);
    statusbar.append(&kind_pill);
    root.append(&statusbar);

    window.set_child(Some(&root));
    // narrow (tiled next to other windows): icons only, except Extract here
    {
        let labelled: Vec<gtk::Widget> = [&open, &new_btn, &to, &add_btn, &del_btn].iter().map(|b| (*b).clone().upcast()).collect();
        let name_w = name.clone();
        let fit = move |w: &gtk::ApplicationWindow| {
            let narrow = w.width() > 0 && w.width() < 820;
            for b in &labelled {
                if let Some(l) = b.first_child().and_then(|bx| bx.last_child()).and_downcast::<gtk::Label>() {
                    l.set_visible(!narrow);
                }
            }
            name_w.set_visible(w.width() == 0 || w.width() >= 560);
        };
        // GTK has no resize signal for toplevels; a light check that only acts on a change
        let (ww, last) = (window.downgrade(), Cell::new(-1));
        glib::timeout_add_local(Duration::from_millis(250), move || {
            let Some(w) = ww.upgrade() else { return glib::ControlFlow::Break };
            if w.width() != last.get() {
                last.set(w.width());
                fit(&w);
            }
            glib::ControlFlow::Continue
        });
    }

    let w = Rc::new(Win {
        window: window.clone(),
        looks: looks::get(),
        arc: RefCell::new(None),
        cwd: Cell::new(0),
        cache: RefCell::new(HashMap::new()),
        loading: Cell::new(0),
        stack,
        store,
        sel,
        view,
        crumbs,
        status,
        kind_pill,
        state_title,
        state_text,
        state_icon,
        state_open: open2.clone(),
        job,
        job_busy: Cell::new(false),
        edit_watch: RefCell::new(vec![]),
        keep_watches: Cell::new(false),
        ctx_menu: {
            let m = gio::Menu::new();
            let a = gio::Menu::new();
            a.append(Some("Open"), Some("win.open-selected"));
            a.append(Some("Extract to…"), Some("win.extract-to"));
            a.append(Some("Extract here"), Some("win.extract-here"));
            m.append_section(None, &a);
            let b = gio::Menu::new();
            b.append(Some("Rename…"), Some("win.rename"));
            b.append(Some("Delete"), Some("win.delete"));
            m.append_section(None, &b);
            let c = gio::Menu::new();
            c.append(Some("Test archive"), Some("win.test"));
            m.append_section(None, &c);
            let pm = gtk::PopoverMenu::from_model(Some(&m));
            pm.set_has_arrow(false);
            pm.set_halign(gtk::Align::Start);
            pm
        },
        extract_btns: vec![sep.upcast(), here.upcast(), to.upcast(), add_btn.upcast(), del_btn.upcast()],
    });
    let weak = Rc::downgrade(&w);
    w.looks.on_change(move || {
        if let Some(w) = weak.upgrade() {
            w.sync_colors_action();
            w.refresh_list();
        }
    });
    w.ctx_menu.set_parent(&w.view);
    w.build_columns();
    w.build_actions(&menu_btn);
    w.build_drop(&root);
    w.wire_job();
    w.show_state("package-x-generic-symbolic", "Drop an archive here", "zip, 7z, rar, tar.gz, tar.zst, iso and many more");
    w.update_actions();

    // the window owns the state
    let keep = RefCell::new(Some(w.clone()));
    window.connect_close_request(move |_| {
        if let Some(w) = keep.borrow_mut().take() {
            w.cache.borrow().values().for_each(|p| p.cancel());
        }
        glib::Propagation::Proceed
    });
    window.present();
    if let Some(f) = file.and_then(|f| f.path()) {
        w.open_archive(f);
    }
    w
}

impl Win {
    // ------------------------------------------------------------------ theme
    fn sync_colors_action(&self) {
        if let Some(a) = self.window.lookup_action("colors").and_downcast::<gio::SimpleAction>() {
            a.set_state(&self.looks.colors_choice().to_variant());
        }
    }

    // ------------------------------------------------------------------ actions
    fn build_actions(self: &Rc<Self>, menu_btn: &gtk::MenuButton) {
        let add = |name: &str, f: fn(&Rc<Win>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            self.window.add_action(&a);
        };
        add("open", |w| w.pick_archive());
        add("extract-here", |w| w.extract_here());
        add("extract-to", |w| w.extract_to_dialog());
        add("up", |w| w.go_up());
        add("new", |w| w.pick_files_to_pack());
        add("add", |w| w.pick_files_to_add());
        add("delete", |w| w.delete_selected());
        add("rename", |w| w.rename_selected());
        add("test", |w| w.test_archive());
        add("open-selected", |w| w.open_selected());
        add("close", |w| w.window.close());

        // Colours: follow the rice, or keep one
        let c = self.looks.colors_choice();
        let colors = gio::SimpleAction::new_stateful("colors", Some(glib::VariantTy::STRING), &c.to_variant());
        let weak = Rc::downgrade(self);
        colors.connect_activate(move |a, v| {
            let (Some(w), Some(v)) = (weak.upgrade(), v.and_then(|v| v.get::<String>())) else { return };
            a.set_state(&v.to_variant());
            let mut cfg = w.looks.cfg.borrow().clone();
            cfg.appearance.colors = v;
            let weak = Rc::downgrade(&w);
            glib::spawn_future_local(async move {
                let saved = gio::spawn_blocking(move || config::save(&cfg).map_err(|e| e.to_string())).await;
                if let (Some(w), Ok(Err(e))) = (weak.upgrade(), saved) {
                    w.job_error("Couldn't save settings", &e);
                }
                // the config monitor re-themes; this covers a missing monitor
                if let Some(w) = weak.upgrade() {
                    w.looks.reload();
                }
            });
        });
        self.window.add_action(&colors);

        let app = self.window.application().expect("app");
        app.set_accels_for_action("win.open", &["<Control>o"]);
        app.set_accels_for_action("win.extract-to", &["<Control>e"]);
        app.set_accels_for_action("win.extract-here", &["<Control><Shift>e"]);
        app.set_accels_for_action("win.up", &["<Alt>Up"]);
        app.set_accels_for_action("win.new", &["<Control>n"]);
        app.set_accels_for_action("win.rename", &["F2"]);
        app.set_accels_for_action("win.test", &["<Control>t"]);
        app.set_accels_for_action("win.close", &["<Control>w", "<Control>q"]);

        // Backspace goes up, unless something takes text
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(w) = weak.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::BackSpace && mods.is_empty() {
                w.go_up();
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Delete && mods.is_empty() {
                w.delete_selected();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.view.add_controller(keys);

        // menu, rebuilt on open so new rices show up
        let weak = Rc::downgrade(self);
        let mb = menu_btn.clone();
        menu_btn.set_create_popup_func(move |_| {
            let Some(w) = weak.upgrade() else { return };
            let menu = gio::Menu::new();
            let colors = gio::Menu::new();
            colors.append(Some("Follow the rice"), Some("win.colors::rice"));
            for (id, name) in theme::list_rices(&w.looks.paths) {
                let item = gio::MenuItem::new(Some(&name), None);
                item.set_action_and_target_value(Some("win.colors"), Some(&id.to_variant()));
                colors.append_item(&item);
            }
            menu.append_submenu(Some("Colours"), &colors);
            let misc = gio::Menu::new();
            if w.arc.borrow().is_some() {
                misc.append(Some("Test archive"), Some("win.test"));
            }
            misc.append(Some("Close"), Some("win.close"));
            menu.append_section(None, &misc);
            mb.set_menu_model(Some(&menu));
        });
    }

    fn update_actions(&self) {
        let open = self.arc.borrow().is_some();
        for name in ["extract-here", "extract-to"] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(open && !self.job_busy.get());
            }
        }
        if let Some(a) = self.window.lookup_action("up").and_downcast::<gio::SimpleAction>() {
            a.set_enabled(open && self.cwd.get() != 0);
        }
        let why_not = self.arc.borrow().as_ref().map(|a| modify::can_modify(&a.listing));
        let can = matches!(why_not, Some(Ok(()))) && !self.job_busy.get();
        let has_sel = !self.sel.selection().is_empty();
        let one = self.selected_nodes().len() == 1;
        for (name, on) in [
            ("add", can),
            ("delete", can && has_sel),
            ("rename", can && one),
            ("new", !self.job_busy.get()),
            ("test", open && !self.job_busy.get()),
            ("open-selected", open && has_sel),
        ] {
            if let Some(a) = self.window.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                a.set_enabled(on);
            }
        }
        // say why Add and Delete are off for rar, iso, ...
        let tip = match &why_not {
            Some(Err(e)) => Some(e.clone()),
            _ => None,
        };
        for b in &self.extract_btns[3..] {
            match &tip {
                Some(t) => b.set_tooltip_text(Some(t)),
                None => b.set_tooltip_text(Some(if b == &self.extract_btns[3] {
                    "Add files into this folder of the archive (or drop them on the window)"
                } else {
                    "Delete the selected items from the archive (Delete)"
                })),
            }
        }
        for b in &self.extract_btns {
            b.set_visible(open);
        }
    }

    // ------------------------------------------------------------------ opening
    fn pick_archive(self: &Rc<Self>) {
        let d = gtk::FileDialog::builder().title("Open an archive").modal(true).build();
        if let Some(dir) = self.arc.borrow().as_ref().and_then(|a| a.path.parent().map(gio::File::for_path)) {
            d.set_initial_folder(Some(&dir));
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(f) = d.open_future(Some(&w.window)).await {
                if let Some(p) = f.path() {
                    w.open_archive(p);
                }
            }
        });
    }

    pub fn open_archive(self: &Rc<Self>, path: PathBuf) {
        self.open_archive_at(path, None);
    }

    /// Open, landing in the folder `at` if it's still there.
    pub fn open_archive_at(self: &Rc<Self>, path: PathBuf, at: Option<String>) {
        let n = self.loading.get() + 1;
        self.loading.set(n);
        let name = file_name(&path);
        if at.is_none() {
            self.show_state("content-loading-symbolic", &format!("Reading {name}…"), "");
        }
        self.window.set_title(Some(&format!("{name} — Unleash")));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let p2 = path.clone();
            let mut r = gio::spawn_blocking(move || backend::list(&p2).map(|l| Opened::new(p2, l, None)))
                .await
                .unwrap_or_else(|_| Err("Reading the archive crashed".into()));
            // names hidden behind a password (7z -mhe): find the password, then list again
            if matches!(&r, Err(e) if e == backend::NEEDS_PASSWORD) {
                let Some(w) = weak.upgrade() else { return };
                let got: Rc<RefCell<Option<Opened>>> = Rc::default();
                let (g2, p3) = (got.clone(), path.clone());
                let res = crate::password::obtain(w.window.upcast_ref(), path.clone(), move |pw| {
                    let (g2, p3) = (g2.clone(), p3.clone());
                    async move {
                        let pw2 = pw.clone();
                        let o = gio::spawn_blocking(move || backend::list_pw(&p3, Some(&pw2)).map(|l| Opened::new(p3, l, Some(pw2.clone()))))
                            .await
                            .unwrap_or_else(|_| Err("crashed".into()))?;
                        *g2.borrow_mut() = Some(o);
                        Ok(())
                    }
                })
                .await;
                r = match (res, got.borrow_mut().take()) {
                    (Ok(_), Some(o)) => Ok(o),
                    (Err(e), _) if e == backend::CANCELLED => Err("It's locked with a password. Open it again to type the password.".into()),
                    (Err(e), _) => Err(e),
                    (Ok(_), None) => Err("Couldn't read it".into()),
                };
            }
            let Some(w) = weak.upgrade() else { return };
            if w.loading.get() != n {
                return; // another archive was opened meanwhile
            }
            w.cache.borrow().values().for_each(|p| p.cancel());
            w.cache.borrow_mut().clear();
            if !w.keep_watches.replace(false) {
                w.edit_watch.borrow_mut().clear();
            }
            match r {
                Ok(o) => {
                    let type_name = o.listing.type_name.clone();
                    *w.arc.borrow_mut() = Some(Arc::new(o));
                    w.kind_pill.set_text(&type_name);
                    w.kind_pill.set_visible(true);
                    let cwd = at.as_deref().and_then(|a| w.arc.borrow().as_ref().and_then(|o| o.tree.find(a))).unwrap_or(0);
                    w.cwd.set(cwd);
                    w.sel.unselect_all();
                    w.stack.set_visible_child_name("list");
                    w.refresh_list();
                }
                Err(e) => {
                    *w.arc.borrow_mut() = None;
                    w.kind_pill.set_visible(false);
                    w.window.set_title(Some("Unleash"));
                    w.show_state("dialog-error-symbolic", &format!("Can't open {name}"), &e);
                }
            }
            w.update_actions();
        });
    }

    fn show_state(&self, icon: &str, title: &str, text: &str) {
        self.state_icon.set_icon_name(Some(icon));
        self.state_title.set_text(title);
        self.state_text.set_text(text);
        self.state_text.set_visible(!text.is_empty());
        // no "Open an archive…" while one is loading
        self.state_open.set_visible(icon != "content-loading-symbolic");
        self.stack.set_visible_child_name("state");
        self.status.set_text("");
    }

    // ------------------------------------------------------------------ list
    fn refresh_list(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let cwd = self.cwd.get();
        let node = &arc.tree.nodes[cwd];
        let items: Vec<glib::BoxedAnyObject> = node.children.iter().map(|&i| glib::BoxedAnyObject::new(Row { arc: arc.clone(), i })).collect();
        self.store.splice(0, self.store.n_items(), &items);
        self.rebuild_crumbs(&arc);
        self.update_status();
        self.update_actions();
        if self.sel.n_items() > 0 {
            self.view.scroll_to(0, None, gtk::ListScrollFlags::NONE, None);
        }
    }

    fn rebuild_crumbs(self: &Rc<Self>, arc: &Arc<Opened>) {
        while let Some(c) = self.crumbs.first_child() {
            self.crumbs.remove(&c);
        }
        let chain = arc.tree.ancestors(self.cwd.get());
        for (k, &i) in chain.iter().enumerate() {
            if k > 0 {
                let sep = gtk::Image::from_icon_name("go-next-symbolic");
                sep.add_css_class("crumb-sep");
                self.crumbs.append(&sep);
            }
            let text = if i == 0 { file_name(&arc.path) } else { arc.tree.nodes[i].name.clone() };
            let b = gtk::Button::with_label(&text);
            b.add_css_class("crumb");
            if k + 1 == chain.len() {
                b.add_css_class("current");
            }
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.enter(i);
                }
            });
            self.crumbs.append(&b);
        }
    }

    fn enter(self: &Rc<Self>, i: usize) {
        self.cwd.set(i);
        self.refresh_list();
        self.view.grab_focus();
    }

    fn go_up(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let cwd = self.cwd.get();
        if cwd != 0 {
            let name = arc.tree.nodes[cwd].name.clone();
            self.enter(arc.tree.nodes[cwd].parent);
            // keyboard focus on the folder we came from (not selected: Extract still means
            // the whole folder being browsed)
            if let Some(pos) = (0..self.sel.n_items()).find(|&k| self.row_at(k).is_some_and(|r| arc.tree.nodes[r].name == name)) {
                self.view.scroll_to(pos, None, gtk::ListScrollFlags::FOCUS, None);
            }
        }
    }

    fn row_at(&self, pos: u32) -> Option<usize> {
        let o = self.sel.item(pos).and_downcast::<glib::BoxedAnyObject>()?;
        let i = o.borrow::<Row>().i;
        Some(i)
    }

    /// Selected node indexes in view order (nothing selected = empty).
    fn selected_nodes(&self) -> Vec<usize> {
        let bs = self.sel.selection();
        let mut v = vec![];
        if let Some((mut it, first)) = gtk::BitsetIter::init_first(&bs) {
            let mut pos = Some(first);
            while let Some(p) = pos {
                if let Some(i) = self.row_at(p) {
                    v.push(i);
                }
                pos = it.next();
            }
        }
        v
    }

    fn update_status(&self) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let sel = self.selected_nodes();
        let t = &arc.tree;
        let node = &t.nodes[self.cwd.get()];
        let txt = if sel.is_empty() {
            let n = node.children.len();
            let mut s = format!("{n} {}", if n == 1 { "item" } else { "items" });
            if node.size > 0 {
                s.push_str(&format!(" · {}", human_size(node.size)));
                if let Some(pk) = node.packed.filter(|&p| p > 0) {
                    s.push_str(&format!(" → {} ({}%)", human_size(pk), (pk as f64 / node.size as f64 * 100.0).round()));
                }
            }
            if self.cwd.get() == 0 {
                let total = t.nodes[0].files;
                if total as usize != n {
                    s.push_str(&format!(" · {total} files in all"));
                }
            }
            s
        } else {
            let size: u64 = sel.iter().map(|&i| t.nodes[i].size).sum();
            format!("{} selected · {}", sel.len(), human_size(size))
        };
        self.status.set_text(&txt);
    }

    fn build_columns(self: &Rc<Self>) {
        // Name
        let f = gtk::SignalListItemFactory::new();
        f.connect_setup(|_, o| {
            let cell = o.downcast_ref::<gtk::ColumnViewCell>().unwrap();
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            b.add_css_class("cell");
            b.add_css_class("name-cell");
            let img = gtk::Image::new();
            img.set_pixel_size(20);
            let l = label("", "");
            l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            l.set_hexpand(true);
            let lock = gtk::Image::from_icon_name("changes-prevent-symbolic");
            lock.add_css_class("lock");
            lock.set_tooltip_text(Some("Encrypted"));
            b.append(&img);
            b.append(&l);
            b.append(&lock);
            cell.set_child(Some(&b));
        });
        let weak = Rc::downgrade(self);
        f.connect_bind(move |_, o| {
            let (Some(w), Some(cell)) = (weak.upgrade(), o.downcast_ref::<gtk::ColumnViewCell>()) else { return };
            let Some(b) = cell.child().and_downcast::<gtk::Box>() else { return };
            let Some(obj) = cell.item().and_downcast::<glib::BoxedAnyObject>() else { return };
            let row = obj.borrow::<Row>();
            let n = &row.arc.tree.nodes[row.i];
            let img = b.first_child().and_downcast::<gtk::Image>().unwrap();
            let l = img.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let lock = l.next_sibling().unwrap();
            img.set_from_gicon(&w.node_icon(n));
            l.set_text(&n.name);
            lock.set_visible(n.encrypted);
        });
        self.add_drag(&f);
        let name_col = gtk::ColumnViewColumn::new(Some("Name"), Some(f));
        name_col.set_expand(true);
        name_col.set_resizable(true);
        name_col.set_sorter(Some(&sorter(|a, b| natural_cmp(&a.name, &b.name))));
        self.view.append_column(&name_col);

        // Size, Packed, Modified
        type Txt = fn(&crate::tree::Node) -> String;
        let cols: [(&str, Txt, fn(&crate::tree::Node, &crate::tree::Node) -> std::cmp::Ordering, i32); 3] = [
            ("Size", |n| if n.is_dir && n.files == 0 { "—".into() } else { human_size(n.size) }, |a, b| a.size.cmp(&b.size), 90),
            ("Packed", |n| n.packed.map(human_size).unwrap_or_else(|| "—".into()), |a, b| a.packed.cmp(&b.packed), 90),
            ("Modified", |n| n.modified.clone(), |a, b| a.modified.cmp(&b.modified), 140),
        ];
        for (title, text, cmp, width) in cols {
            let f = gtk::SignalListItemFactory::new();
            f.connect_setup(|_, o| {
                let cell = o.downcast_ref::<gtk::ColumnViewCell>().unwrap();
                let l = label("", "file-meta");
                l.add_css_class("cell");
                l.add_css_class("numeric");
                cell.set_child(Some(&l));
            });
            f.connect_bind(move |_, o| {
                let cell = o.downcast_ref::<gtk::ColumnViewCell>().unwrap();
                let (Some(l), Some(obj)) = (cell.child().and_downcast::<gtk::Label>(), cell.item().and_downcast::<glib::BoxedAnyObject>()) else { return };
                let row = obj.borrow::<Row>();
                l.set_text(&text(&row.arc.tree.nodes[row.i]));
                l.set_xalign(if title == "Modified" { 0.0 } else { 1.0 });
            });
            self.add_drag(&f);
            let col = gtk::ColumnViewColumn::new(Some(title), Some(f));
            col.set_fixed_width(width);
            col.set_resizable(true);
            col.set_sorter(Some(&sorter(cmp)));
            self.view.append_column(&col);
        }
        self.view.sort_by_column(Some(&name_col), gtk::SortType::Ascending);

        let weak = Rc::downgrade(self);
        self.view.connect_activate(move |_, pos| {
            let Some(w) = weak.upgrade() else { return };
            let Some(i) = w.row_at(pos) else { return };
            let Some(arc) = w.arc.borrow().clone() else { return };
            if arc.tree.nodes[i].is_dir {
                w.enter(i);
            } else {
                w.open_in_app(i);
            }
        });
        let weak = Rc::downgrade(self);
        self.sel.connect_selection_changed(move |_, _, _| {
            if let Some(w) = weak.upgrade() {
                w.update_status();
                w.update_actions();
            }
        });
    }

    // ------------------------------------------------------------------ drag out
    fn add_drag(self: &Rc<Self>, f: &gtk::SignalListItemFactory) {
        let weak = Rc::downgrade(self);
        f.connect_setup(move |_, o| {
            let cell = o.downcast_ref::<gtk::ColumnViewCell>().unwrap();
            let Some(child) = cell.child() else { return };
            let src = gtk::DragSource::new();
            src.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
            // what the current drag carries: set in prepare, used by begin and cancel
            let cur: Rc<RefCell<Option<(Rc<Pending>, gio::Icon, String, usize)>>> = Rc::default();
            let (cw, ww, c) = (cell.downgrade(), weak.clone(), cur.clone());
            src.connect_prepare(move |_, _, _| {
                let (w, cell) = (ww.upgrade()?, cw.upgrade()?);
                let arc = w.arc.borrow().clone()?;
                let mut nodes = w.selected_nodes();
                let here = cell.item().and_downcast::<glib::BoxedAnyObject>().map(|o| o.borrow::<Row>().i)?;
                if !cell.is_selected() || nodes.is_empty() {
                    nodes = vec![here];
                    w.sel.select_item(cell.position(), true);
                }
                let paths: Vec<String> = nodes.iter().map(|&i| arc.tree.nodes[i].path.clone()).collect();
                // locked: a drag can't wait for a dialog, so ask now and let the user drag again
                if backend::needs_password(&arc.listing, &paths) && arc.password().is_none() {
                    let w2 = w.clone();
                    glib::spawn_future_local(async move {
                        if let Ok(Some(_)) = w2.unlock(&arc, &paths).await {
                            w2.job_note("Unlocked", "Drag it again");
                        }
                    });
                    return None;
                }
                let pending = w.extract_cached(&arc, paths);
                let n = &arc.tree.nodes[here];
                let icon = w.node_icon(n);
                w.watch_pending(&pending, nodes.len(), &n.name);
                *c.borrow_mut() = Some((pending.clone(), icon, n.name.clone(), nodes.len()));
                Some(LazyFiles::new(pending).upcast())
            });
            // the thing under the cursor while dragging: icon + name, like file managers do
            let c = cur.clone();
            src.connect_drag_begin(move |_, drag| {
                let Some((_, icon, name, count)) = c.borrow().clone() else { return };
                let chip = gtk::Box::new(gtk::Orientation::Horizontal, 8);
                chip.add_css_class("drag-chip");
                let overlay = gtk::Overlay::new();
                let img = gtk::Image::from_gicon(&icon);
                img.set_pixel_size(32);
                overlay.set_child(Some(&img));
                if count > 1 {
                    let badge = gtk::Label::new(Some(&count.to_string()));
                    badge.add_css_class("badge");
                    badge.set_halign(gtk::Align::End);
                    badge.set_valign(gtk::Align::End);
                    overlay.add_overlay(&badge);
                }
                chip.append(&overlay);
                let text = if count > 1 { format!("{name} and {} more", count - 1) } else { name };
                let l = gtk::Label::new(Some(&text));
                l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                l.set_max_width_chars(32);
                chip.append(&l);
                gtk::DragIcon::for_drag(drag).set_child(Some(&chip));
                // a little below-right of the pointer, so the drop spot stays visible
                drag.set_hotspot(-14, -14);
            });
            let (c, ww) = (cur, weak.clone());
            src.connect_drag_cancel(move |_, _, _| {
                // dropped nowhere: stop extracting if nobody has the files yet
                if let (Some(w), Some((p, ..))) = (ww.upgrade(), c.borrow_mut().take()) {
                    if p.result().is_none() {
                        p.cancel();
                        w.cache.borrow_mut().retain(|_, v| !Rc::ptr_eq(v, &p));
                    }
                }
                false
            });
            child.add_controller(src);

            // right click: select the row (unless it's already part of the selection), show the menu
            let rc = gtk::GestureClick::new();
            rc.set_button(gdk::BUTTON_SECONDARY);
            let (cw, ww) = (cell.downgrade(), weak.clone());
            rc.connect_pressed(move |g, _, x, y| {
                let (Some(w), Some(cell)) = (ww.upgrade(), cw.upgrade()) else { return };
                g.set_state(gtk::EventSequenceState::Claimed);
                if !cell.is_selected() {
                    w.sel.select_item(cell.position(), true);
                }
                let Some(widget) = g.widget() else { return };
                if let Some(pt) = widget.compute_point(&w.view, &gtk::graphene::Point::new(x as f32, y as f32)) {
                    w.ctx_menu.set_pointing_to(Some(&gdk::Rectangle::new(pt.x() as i32, pt.y() as i32, 1, 1)));
                }
                w.update_actions();
                w.ctx_menu.popup();
            });
            child.add_controller(rc);
        });
    }

    /// Enter the selected folder, or open the first selected file in its app.
    fn open_selected(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let Some(&i) = self.selected_nodes().first() else { return };
        if arc.tree.nodes[i].is_dir {
            self.enter(i);
        } else {
            self.open_in_app(i);
        }
    }

    fn rename_selected(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let sel = self.selected_nodes();
        let [i] = sel[..] else { return };
        if let Err(e) = modify::can_modify(&arc.listing) {
            self.job_error("Can't rename in this archive", &e);
            return;
        }
        let (from, name, is_dir) = (arc.tree.nodes[i].path.clone(), arc.tree.nodes[i].name.clone(), arc.tree.nodes[i].is_dir);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            let Some(new) = crate::report::ask_name(w.window.upcast_ref(), &name, is_dir).await else { return };
            let a2 = arc.clone();
            w.run_job(
                format!("Renaming {name}"),
                format!("Couldn't rename {name}"),
                move |p| modify::rename(&a2.path, &a2.listing, &from, &new, p),
                move |w, ()| {
                    w.job_hide();
                    w.reopen();
                },
            );
        });
    }

    fn test_archive(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let name = file_name(&arc.path);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            let pw = match w.unlock(&arc, &[]).await {
                Ok(pw) => pw,
                Err(e) if e == backend::CANCELLED => return,
                Err(e) => return w.job_error(&format!("Couldn't test {name}"), &e),
            };
            let n2 = name.clone();
            w.run_job(
                format!("Testing {name}"),
                format!("Couldn't test {name}"),
                move |p| backend::check::test(&arc.path, &arc.listing, pw.as_deref(), p),
                move |w, report| {
                    w.job_hide();
                    crate::report::show_test(w.window.upcast_ref(), &n2, &report);
                },
            );
        });
    }

    /// After a file from the archive was opened in its app: when it's saved there, offer to put
    /// it back. Editors often save by writing a new file and renaming it over the old one, so
    /// the folder is watched, not the file.
    fn watch_for_edits(self: &Rc<Self>, arc: &Arc<Opened>, entry: String, file: PathBuf) {
        if modify::can_modify(&arc.listing).is_err() {
            return;
        }
        let (Some(dir), Some(fname)) = (file.parent(), file.file_name().map(|n| n.to_os_string())) else { return };
        let Ok(m) = gio::File::for_path(dir).monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE) else { return };
        let stamp = |f: &Path| std::fs::metadata(f).and_then(|m| m.modified()).ok();
        let last = Rc::new(RefCell::new(stamp(&file)));
        let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let (weak, arc_path) = (Rc::downgrade(self), arc.path.clone());
        m.connect_changed(move |_, f, other, ev| {
            use gio::FileMonitorEvent as E;
            let hit = |x: Option<&gio::File>| x.and_then(|x| x.basename()).is_some_and(|b| b.as_os_str() == fname);
            let ours = match ev {
                E::ChangesDoneHint | E::Created | E::Changed => hit(Some(f)),
                E::MovedIn | E::Renamed => hit(other) || hit(Some(f)),
                _ => false,
            };
            if !ours {
                return;
            }
            // wait until the app has finished writing
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let (weak, file, entry, last, arc_path, pend) = (weak.clone(), file.clone(), entry.clone(), last.clone(), arc_path.clone(), pending.clone());
            let id = glib::timeout_add_local_once(Duration::from_millis(1200), move || {
                *pend.borrow_mut() = None;
                let now = stamp(&file);
                if now.is_none() || *last.borrow() == now {
                    return;
                }
                *last.borrow_mut() = now;
                let Some(w) = weak.upgrade() else { return };
                // only while that archive is still the open one
                if w.arc.borrow().as_ref().map(|a| a.path.clone()) != Some(arc_path.clone()) {
                    return;
                }
                w.offer_update(entry.clone(), file.clone());
            });
            *pending.borrow_mut() = Some(id);
        });
        self.edit_watch.borrow_mut().push(m);
    }

    fn offer_update(self: &Rc<Self>, entry: String, file: PathBuf) {
        let name = entry.rsplit('/').next().unwrap_or(&entry).to_string();
        let Some(arc) = self.arc.borrow().clone() else { return };
        let what = file_name(&arc.path);
        let weak = Rc::downgrade(self);
        self.job_ask(&format!("{name} was changed"), &format!("Put the new version into {what}?"), "Update", move || {
            let Some(w) = weak.upgrade() else { return };
            let Some(arc) = w.arc.borrow().clone() else { return };
            let base = entry.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or_default();
            let (file, a2, name2, what2) = (file.clone(), arc.clone(), name.clone(), what.clone());
            w.run_job(
                format!("Updating {name}"),
                format!("Couldn't update {name}"),
                move |p| modify::add(&a2.path, &a2.listing, std::slice::from_ref(&file), &base, p),
                move |w, ()| {
                    w.job_note(&format!("Updated {name2}"), &format!("in {what2}"));
                    w.reopen_keeping_watches();
                },
            );
        });
    }

    /// The same icon the list shows: a rice-coloured folder, or the file type's icon.
    fn node_icon(&self, n: &crate::tree::Node) -> gio::Icon {
        if n.is_dir {
            // GTK searches a child theme fully before its parents, so ["folder-carmine", "folder"]
            // would give Papirus-Catppuccin's plain (violet) folder: pick the name ourselves, like Burrow
            let c = theme::papirus_folder_color(&self.looks.tokens.borrow());
            let tinted = format!("folder-{c}");
            let has = gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(&tinted));
            return gio::ThemedIcon::new(if has { &tinted } else { "folder" }).upcast();
        }
        gio::content_type_get_icon(&gio::content_type_guess(Some(&n.name), None).0)
    }

    /// Start (or reuse) extracting `paths` into the drag cache.
    fn extract_cached(self: &Rc<Self>, arc: &Arc<Opened>, mut paths: Vec<String>) -> Rc<Pending> {
        paths.sort();
        if let Some(p) = self.cache.borrow().get(&paths) {
            let ok = match p.result() {
                Some(Ok(files)) => files.iter().all(|f| f.symlink_metadata().is_ok()),
                Some(Err(_)) => false,
                None => true,
            };
            if ok {
                return p.clone();
            }
        }
        let p = drag::start(arc.clone(), paths.clone());
        self.cache.borrow_mut().insert(paths, p.clone());
        p
    }

    /// Show progress for a drag extract in the job strip when it takes a while.
    fn watch_pending(self: &Rc<Self>, p: &Rc<Pending>, count: usize, name: &str) {
        if p.result().is_some() || self.job_busy.get() {
            return;
        }
        let what = if count > 1 { format!("{count} items") } else { name.to_string() };
        let (weak, pw) = (Rc::downgrade(self), Rc::downgrade(p));
        let shown = Rc::new(Cell::new(false));
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let (Some(w), Some(p)) = (weak.upgrade(), pw.upgrade()) else { return glib::ControlFlow::Break };
            if w.job_busy.get() && !shown.get() {
                return glib::ControlFlow::Break;
            }
            match p.result() {
                None => {
                    if !shown.get() && p.progress.fraction() < 0.5 {
                        shown.set(true);
                        w.job_start(&format!("Getting {what} ready to drop…"), p.progress.clone());
                    }
                    if shown.get() {
                        w.job_tick(&p.progress);
                    }
                    glib::ControlFlow::Continue
                }
                Some(r) => {
                    if shown.get() {
                        match r {
                            Ok(_) => w.job_hide(),
                            Err(e) if e == backend::CANCELLED => w.job_hide(),
                            Err(e) => w.job_error(&format!("Couldn't extract {what}"), &e),
                        }
                    } else if let Err(e) = r {
                        if e != backend::CANCELLED {
                            w.job_error(&format!("Couldn't extract {what}"), &e);
                        }
                    }
                    glib::ControlFlow::Break
                }
            }
        });
    }

    /// Double-click on a file: extract to the cache, open with its default app.
    fn open_in_app(self: &Rc<Self>, i: usize) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let (path, name) = (arc.tree.nodes[i].path.clone(), arc.tree.nodes[i].name.clone());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            match w.unlock(&arc, std::slice::from_ref(&path)).await {
                Ok(_) => {}
                Err(e) if e == backend::CANCELLED => return,
                Err(e) => return w.job_error(&format!("Couldn't open {name}"), &e),
            }
            let path2 = path.clone();
            let p = w.extract_cached(&arc, vec![path]);
            w.watch_pending(&p, 1, &name);
            let Ok(files) = p.wait().await else { return };
            let Some(f) = files.first() else { return };
            w.watch_for_edits(&arc, path2.clone(), f.clone());
            let l = gtk::FileLauncher::new(Some(&gio::File::for_path(f)));
            if let Err(e) = l.launch_future(Some(&w.window)).await {
                if !e.matches(gtk::DialogError::Dismissed) && !e.matches(gtk::DialogError::Cancelled) {
                    w.job_error("Couldn't open it", e.message());
                }
            }
        });
    }

    // ------------------------------------------------------------------ drop in
    fn build_drop(self: &Rc<Self>, root: &gtk::Box) {
        let t = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let rw = root.downgrade();
        t.connect_enter(move |t, _, _| {
            if t.current_drop().and_then(|d| d.drag()).is_some() {
                return gdk::DragAction::empty(); // our own drag
            }
            if let Some(r) = rw.upgrade() {
                r.add_css_class("drop-hover");
            }
            gdk::DragAction::COPY
        });
        let rw = root.downgrade();
        t.connect_leave(move |_| {
            if let Some(r) = rw.upgrade() {
                r.remove_css_class("drop-hover");
            }
        });
        let (weak, rw) = (Rc::downgrade(self), root.downgrade());
        t.connect_drop(move |t, v, _, _| {
            if let Some(r) = rw.upgrade() {
                r.remove_css_class("drop-hover");
            }
            if t.current_drop().and_then(|d| d.drag()).is_some() {
                return false;
            }
            let (Some(w), Ok(list)) = (weak.upgrade(), v.get::<gdk::FileList>()) else { return false };
            let files: Vec<PathBuf> = list.files().iter().filter_map(|f| f.path()).collect();
            if files.is_empty() {
                return false;
            }
            // an archive is open: add to it. Nothing open: one archive opens, anything else
            // becomes a new archive.
            if w.arc.borrow().is_some() {
                w.add_files(files);
            } else if files.len() == 1 && files[0].is_file() && looks_like_archive(&files[0]) {
                w.open_archive(files[0].clone());
            } else {
                crate::create_dialog::show(&w, files);
            }
            true
        });
        root.add_controller(t);
    }

    // ------------------------------------------------------------------ extract
    /// What to extract: the selection, or everything in the folder being browsed.
    fn extract_targets(&self, arc: &Opened) -> (Vec<String>, Placement) {
        let sel = self.selected_nodes();
        let cwd = self.cwd.get();
        if sel.is_empty() {
            if cwd == 0 {
                return (vec![], Placement::Smart);
            }
            return (vec![arc.tree.nodes[cwd].path.clone()], Placement::Loose);
        }
        (sel.iter().map(|&i| arc.tree.nodes[i].path.clone()).collect(), Placement::Loose)
    }

    fn extract_here(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let Some(dir) = arc.path.parent().map(Path::to_path_buf) else { return };
        self.run_extract(arc, dir);
    }

    fn extract_to_dialog(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let d = gtk::FileDialog::builder().title("Extract into").modal(true).accept_label("Extract here").build();
        if let Some(dir) = arc.path.parent() {
            d.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(f) = d.select_folder_future(Some(&w.window)).await {
                if let Some(p) = f.path() {
                    w.run_extract(arc, p);
                }
            }
        });
    }

    fn run_extract(self: &Rc<Self>, arc: Arc<Opened>, dest: PathBuf) {
        if self.job_busy.get() {
            return;
        }
        let (sel, how) = self.extract_targets(&arc);
        // a folder picked while browsing inside it: its contents, under its own name
        let (sel, base, how) = if sel.len() == 1 && how == Placement::Loose {
            let i = arc.tree.find(&sel[0]).unwrap_or(0);
            let parent = arc.tree.nodes[arc.tree.nodes[i].parent].path.clone();
            (sel, parent, Placement::Loose)
        } else {
            let base = arc.tree.nodes[self.cwd.get()].path.clone();
            (sel, base, how)
        };
        let name = file_name(&arc.path);
        let weak = Rc::downgrade(self);
        let sel2 = sel.clone();
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            let pw = match w.unlock(&arc, &sel2).await {
                Ok(pw) => pw,
                Err(e) if e == backend::CANCELLED => return,
                Err(e) => return w.job_error(&format!("Couldn't extract {name}"), &e),
            };
            let d2 = dest.clone();
            w.run_job(
                format!("Extracting {name}"),
                format!("Couldn't extract {name}"),
                move |p| backend::extract_to_pw(&arc.path, &arc.listing, &sel, &base, &d2, how, p, pw.as_deref()),
                move |w, made| w.job_done(&made, &dest),
            );
        });
    }

    /// The password `sel` needs (None when it needs none): known, from the keyring, or asked.
    pub async fn unlock(self: &Rc<Self>, arc: &Arc<Opened>, sel: &[String]) -> Result<Option<String>, String> {
        if !backend::needs_password(&arc.listing, sel) {
            return Ok(None);
        }
        if let Some(pw) = arc.password() {
            return Ok(Some(pw));
        }
        let a2 = arc.clone();
        let pw = crate::password::obtain(self.window.upcast_ref(), arc.path.clone(), move |pw| {
            let a2 = a2.clone();
            async move { gio::spawn_blocking(move || backend::check_password(&a2.path, &a2.listing, &pw)).await.unwrap_or_else(|_| Err("crashed".into())) }
        })
        .await?;
        if let Ok(mut p) = arc.password.lock() {
            *p = Some(pw.clone());
        }
        Ok(Some(pw))
    }

    /// Run `work` on a worker thread with the job strip showing its progress; `done` runs on
    /// the UI thread when it worked. Errors and cancels are shown here.
    pub fn run_job<T: Send + 'static>(
        self: &Rc<Self>,
        title: String,
        fail_title: String,
        work: impl FnOnce(&Progress) -> Result<T, String> + Send + 'static,
        done: impl FnOnce(&Rc<Win>, T) + 'static,
    ) {
        if self.job_busy.get() {
            self.job_error("Busy", "Wait until the current job is done");
            return;
        }
        let progress = Arc::new(Progress::default());
        self.job_busy.set(true);
        self.update_actions();
        self.job_start(&title, progress.clone());
        let (weak, prog) = (Rc::downgrade(self), progress.clone());
        let ticker = glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return glib::ControlFlow::Break };
            w.job_tick(&prog);
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let r = gio::spawn_blocking(move || work(&progress)).await.unwrap_or_else(|_| Err("It crashed".into()));
            ticker.remove();
            let Some(w) = weak.upgrade() else { return };
            w.job_busy.set(false);
            w.update_actions();
            match r {
                Ok(v) => done(&w, v),
                Err(e) if e == backend::CANCELLED => w.job_hide(),
                Err(e) => w.job_error(&fail_title, &e),
            }
        });
    }

    // ------------------------------------------------------------------ change the archive
    /// Add files into the folder being browsed (asks before replacing).
    pub fn add_files(self: &Rc<Self>, files: Vec<PathBuf>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        if files.is_empty() {
            return;
        }
        if let Err(e) = modify::can_modify(&arc.listing) {
            self.job_error("Can't add to this archive", &e);
            return;
        }
        if files.iter().any(|f| *f == arc.path) {
            self.job_error("Can't add an archive to itself", "");
            return;
        }
        let base = arc.tree.nodes[self.cwd.get()].path.clone();
        let clashes = modify::clashes(&arc.listing, &files, &base);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            if !clashes.is_empty() {
                let names: Vec<&str> = clashes.iter().map(|c| c.rsplit('/').next().unwrap_or(c)).collect();
                let msg = if names.len() == 1 { format!("Replace “{}”?", names[0]) } else { format!("Replace {} items?", names.len()) };
                let detail = format!("The archive already has {}. The ones inside will be replaced by yours.", names.join(", "));
                if !crate::confirm::ask(w.window.upcast_ref(), &msg, &detail, "Replace", false).await {
                    return;
                }
            }
            let what = if files.len() == 1 { file_name(&files[0]) } else { format!("{} items", files.len()) };
            let arc2 = arc.clone();
            w.run_job(
                format!("Adding {what}"),
                format!("Couldn't add {what}"),
                move |p| modify::add(&arc2.path, &arc2.listing, &files, &base, p),
                move |w, ()| {
                    w.job_hide();
                    w.reopen();
                },
            );
        });
    }

    fn delete_selected(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let sel = self.selected_nodes();
        if sel.is_empty() {
            return;
        }
        if let Err(e) = modify::can_modify(&arc.listing) {
            self.job_error("Can't delete from this archive", &e);
            return;
        }
        let paths: Vec<String> = sel.iter().map(|&i| arc.tree.nodes[i].path.clone()).collect();
        let what = if sel.len() == 1 { format!("“{}”", arc.tree.nodes[sel[0]].name) } else { format!("{} items", sel.len()) };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            let title = format!("Delete {what} from {}?", file_name(&arc.path));
            if !crate::confirm::ask(w.window.upcast_ref(), &title, "They are removed from the archive for good. Your other files aren't touched.", "Delete", true).await {
                return;
            }
            let arc2 = arc.clone();
            w.run_job(
                format!("Deleting {what}"),
                format!("Couldn't delete {what}"),
                move |p| modify::delete(&arc2.path, &arc2.listing, &paths, p),
                move |w, ()| {
                    w.job_hide();
                    w.reopen();
                },
            );
        });
    }

    /// Read the archive again after a change, staying in the same folder.
    fn reopen(self: &Rc<Self>) {
        let Some(arc) = self.arc.borrow().clone() else { return };
        let here = arc.tree.nodes[self.cwd.get()].path.clone();
        self.open_archive_at(arc.path.clone(), Some(here));
    }

    /// Re-read after putting an edited file back: its app still has it open, keep watching.
    fn reopen_keeping_watches(self: &Rc<Self>) {
        self.keep_watches.set(true);
        self.reopen();
    }

    fn pick_files_to_add(self: &Rc<Self>) {
        let d = gtk::FileDialog::builder().title("Add to the archive").modal(true).accept_label("Add").build();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(list) = d.open_multiple_future(Some(&w.window)).await {
                let files: Vec<PathBuf> = (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
                w.add_files(files);
            }
        });
    }

    fn pick_files_to_pack(self: &Rc<Self>) {
        let d = gtk::FileDialog::builder().title("Files for the new archive").modal(true).accept_label("Next").build();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            if let Ok(list) = d.open_multiple_future(Some(&w.window)).await {
                let files: Vec<PathBuf> = (0..list.n_items()).filter_map(|i| list.item(i).and_downcast::<gio::File>()).filter_map(|f| f.path()).collect();
                if !files.is_empty() {
                    crate::create_dialog::show(&w, files);
                }
            }
        });
    }

    // ------------------------------------------------------------------ job strip
    fn wire_job(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.job.close.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                w.job_hide();
            }
        });
    }

    fn job_start(&self, title: &str, p: Arc<Progress>) {
        let j = &self.job;
        j.root.remove_css_class("error");
        j.root.remove_css_class("done");
        j.title.set_text(title);
        j.detail.set_text("");
        j.bar.set_fraction(0.0);
        j.bar.set_visible(true);
        j.cancel.set_visible(true);
        j.open.set_visible(false);
        j.close.set_visible(false);
        set_handler(&j.cancel, move |_| p.cancel.store(true, Ordering::Relaxed));
        j.revealer.set_reveal_child(true);
    }

    fn job_tick(&self, p: &Progress) {
        let f = p.fraction();
        self.job.bar.set_fraction(f);
        let cur = p.current.lock().map(|c| c.clone()).unwrap_or_default();
        let (done, total) = (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed));
        let cur = cur.rsplit('/').next().unwrap_or("").to_string();
        self.job.detail.set_text(&format!("{} of {} · {}", human_size(done.min(total)), human_size(total), cur));
    }

    pub fn job_done(self: &Rc<Self>, made: &[PathBuf], dest: &Path) {
        let j = &self.job;
        j.root.add_css_class("done");
        let what = match made {
            [one] => file_name(one),
            v => format!("{} items", v.len()),
        };
        j.title.set_text(&format!("Extracted {what}"));
        j.detail.set_text(&format!("into {}", tilde(dest)));
        j.bar.set_visible(false);
        j.cancel.set_visible(false);
        j.close.set_visible(true);
        j.open.set_visible(true);
        j.open.set_label("Show");
        let first = made.first().cloned();
        let weak = Rc::downgrade(self);
        set_handler(&j.open, move |_| {
            let (Some(w), Some(f)) = (weak.upgrade(), first.clone()) else { return };
            // shows the new item selected in the file manager (Burrow, via FileManager1)
            let l = gtk::FileLauncher::new(Some(&gio::File::for_path(&f)));
            let win = w.window.clone();
            glib::spawn_future_local(async move {
                let _ = l.open_containing_folder_future(Some(&win)).await;
            });
        });
        j.revealer.set_reveal_child(true);
    }

    pub fn job_error(&self, title: &str, e: &str) {
        let j = &self.job;
        j.root.remove_css_class("done");
        j.root.add_css_class("error");
        j.title.set_text(title);
        j.detail.set_text(e);
        j.bar.set_visible(false);
        j.cancel.set_visible(false);
        j.open.set_visible(false);
        j.close.set_visible(true);
        j.revealer.set_reveal_child(true);
    }

    /// A question in the job strip: `label` runs `f`, the × dismisses it.
    pub fn job_ask(self: &Rc<Self>, title: &str, detail: &str, label: &str, f: impl Fn() + 'static) {
        if self.job_busy.get() {
            return;
        }
        let j = &self.job;
        j.root.remove_css_class("error");
        j.root.remove_css_class("done");
        j.title.set_text(title);
        j.detail.set_text(detail);
        j.bar.set_visible(false);
        j.cancel.set_visible(false);
        j.close.set_visible(true);
        j.open.set_label(label);
        j.open.set_visible(true);
        set_handler(&j.open, move |_| f());
        j.revealer.set_reveal_child(true);
    }

    /// A short note in the job strip that goes away by itself.
    pub fn job_note(self: &Rc<Self>, title: &str, detail: &str) {
        if self.job_busy.get() {
            return;
        }
        self.job_done_text(title, detail);
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_secs(3), move || {
            if let Some(w) = weak.upgrade() {
                if !w.job_busy.get() {
                    w.job_hide();
                }
            }
        });
    }

    fn job_done_text(&self, title: &str, detail: &str) {
        let j = &self.job;
        j.root.remove_css_class("error");
        j.root.add_css_class("done");
        j.title.set_text(title);
        j.detail.set_text(detail);
        j.bar.set_visible(false);
        j.cancel.set_visible(false);
        j.open.set_visible(false);
        j.close.set_visible(true);
        j.revealer.set_reveal_child(true);
    }

    pub fn job_hide(&self) {
        self.job.revealer.set_reveal_child(false);
    }
}

/// One list row: a node of the open archive's tree.
struct Row {
    arc: Arc<Opened>,
    i: usize,
}

/// Folders first, then `cmp` (the column view flips it for descending, folders included).
fn sorter(cmp: fn(&crate::tree::Node, &crate::tree::Node) -> std::cmp::Ordering) -> gtk::CustomSorter {
    gtk::CustomSorter::new(move |a, b| {
        let (Some(a), Some(b)) = (a.downcast_ref::<glib::BoxedAnyObject>(), b.downcast_ref::<glib::BoxedAnyObject>()) else {
            return gtk::Ordering::Equal;
        };
        let (a, b) = (a.borrow::<Row>(), b.borrow::<Row>());
        let (na, nb) = (&a.arc.tree.nodes[a.i], &b.arc.tree.nodes[b.i]);
        nb.is_dir.cmp(&na.is_dir).then_with(|| cmp(na, nb)).then_with(|| natural_cmp(&na.name, &nb.name)).into()
    })
}

thread_local! {
    static HANDLERS: RefCell<HashMap<String, glib::SignalHandlerId>> = RefCell::new(HashMap::new());
}

/// Replace a button's click handler (the job strip's buttons do different things per job).
fn set_handler(b: &gtk::Button, f: impl Fn(&gtk::Button) + 'static) {
    let key = format!("{:p}", b.as_ptr());
    let id = b.connect_clicked(f);
    HANDLERS.with(|h| {
        if let Some(old) = h.borrow_mut().insert(key, id) {
            b.disconnect(old);
        }
    });
}

/// By name only (no reading on the UI thread): "x.zip", "y.tar.gz", "z.7z.001".
fn looks_like_archive(p: &Path) -> bool {
    let n = file_name(p);
    backend::stem(&n) != n
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn tilde(p: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match p.strip_prefix(&home) {
        Ok(r) if !home.as_os_str().is_empty() => format!("~/{}", r.display()),
        _ => p.display().to_string(),
    }
}
