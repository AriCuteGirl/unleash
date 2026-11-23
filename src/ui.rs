//! The window: toolbar, path bar inside the archive, file list, progress strip, status bar.
//! All reading and extracting happens on worker threads; the UI only polls progress.

use crate::backend::{self, Placement, Progress};
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
    job: JobBar,
    job_busy: Cell<bool>,
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

pub fn open_window(app: &gtk::Application, file: Option<gio::File>) {
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Unleash")
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
    toolbar.append(&here);
    toolbar.append(&to);
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
        job,
        job_busy: Cell::new(false),
        extract_btns: vec![here.upcast(), to.upcast()],
    });
    let weak = Rc::downgrade(&w);
    w.looks.on_change(move || {
        if let Some(w) = weak.upgrade() {
            w.sync_colors_action();
            w.refresh_list();
        }
    });
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
        let n = self.loading.get() + 1;
        self.loading.set(n);
        let name = file_name(&path);
        self.show_state("content-loading-symbolic", &format!("Reading {name}…"), "");
        self.window.set_title(Some(&format!("{name} — Unleash")));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let p2 = path.clone();
            let r = gio::spawn_blocking(move || {
                backend::list(&p2).map(|listing| {
                    let tree = crate::tree::Tree::build(&listing.entries);
                    Opened { path: p2, listing, tree }
                })
            })
            .await
            .unwrap_or_else(|_| Err("Reading the archive crashed".into()));
            let Some(w) = weak.upgrade() else { return };
            if w.loading.get() != n {
                return; // another archive was opened meanwhile
            }
            w.cache.borrow().values().for_each(|p| p.cancel());
            w.cache.borrow_mut().clear();
            match r {
                Ok(o) => {
                    let type_name = o.listing.type_name.clone();
                    *w.arc.borrow_mut() = Some(Arc::new(o));
                    w.kind_pill.set_text(&type_name);
                    w.kind_pill.set_visible(true);
                    w.cwd.set(0);
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
        let n = &arc.tree.nodes[i];
        let p = self.extract_cached(&arc, vec![n.path.clone()]);
        self.watch_pending(&p, 1, &n.name.clone());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(files) = p.wait().await else { return };
            let Some(w) = weak.upgrade() else { return };
            let Some(f) = files.first() else { return };
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
            // adding files to an archive comes in the next milestone: for now a dropped file opens
            match list.files().first().and_then(|f| f.path()) {
                Some(p) if p.is_file() => {
                    w.open_archive(p);
                    true
                }
                _ => false,
            }
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
        let progress = Arc::new(Progress::default());
        let name = file_name(&arc.path);
        self.job_busy.set(true);
        self.update_actions();
        self.job_start(&format!("Extracting {name}"), progress.clone());
        let (weak, prog) = (Rc::downgrade(self), progress.clone());
        let ticker = glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(w) = weak.upgrade() else { return glib::ControlFlow::Break };
            w.job_tick(&prog);
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let d2 = dest.clone();
            let r = gio::spawn_blocking(move || backend::extract_to(&arc.path, &arc.listing, &sel, &base, &d2, how, &progress))
                .await
                .unwrap_or_else(|_| Err("Extracting crashed".into()));
            ticker.remove();
            let Some(w) = weak.upgrade() else { return };
            w.job_busy.set(false);
            w.update_actions();
            match r {
                Ok(made) => w.job_done(&made, &dest),
                Err(e) if e == backend::CANCELLED => w.job_hide(),
                Err(e) => w.job_error(&format!("Couldn't extract {name}"), &e),
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

    fn job_done(self: &Rc<Self>, made: &[PathBuf], dest: &Path) {
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

    fn job_error(&self, title: &str, e: &str) {
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

    fn job_hide(&self) {
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

fn file_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn tilde(p: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    match p.strip_prefix(&home) {
        Ok(r) if !home.as_os_str().is_empty() => format!("~/{}", r.display()),
        _ => p.display().to_string(),
    }
}
