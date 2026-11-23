//! Dragging files out of an archive into other apps.
//!
//! Why this usually fails on Wayland: archive managers made for X11 use XDND Direct Save, where
//! the drop target tells the source which folder it dropped into and the source extracts there.
//! Wayland has no such protocol, so their drops hand out paths that don't exist yet (or nothing).
//!
//! What we do instead:
//! - extracting starts the moment the drag starts, into ~/.cache/unleash/drag/<n>/
//! - the drag offers text/uri-list (browsers, Electron, file managers, XWayland apps through the
//!   compositor), application/vnd.portal.filetransfer (sandboxed apps; GTK registers the files
//!   with the document portal) and text/plain paths (editors, terminals)
//! - when the target asks for the data, the answer waits until the files are complete, so it
//!   never gets a half-written file
//! - the files stay for a day, because browsers and chat apps read them after the drop (while
//!   uploading); older drag folders are removed at startup

use crate::backend::{self, Listing, Progress};
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// An opened archive, shared with worker threads.
pub struct Opened {
    pub path: PathBuf,
    pub listing: Listing,
    pub tree: crate::tree::Tree,
}

/// Files being extracted for a drag (or for opening in an app).
pub struct Pending {
    pub progress: Arc<Progress>,
    result: RefCell<Option<Result<Vec<PathBuf>, String>>>,
}

impl Pending {
    pub fn ready(files: Vec<PathBuf>) -> Rc<Pending> {
        Rc::new(Pending { progress: Arc::default(), result: RefCell::new(Some(Ok(files))) })
    }
    pub fn result(&self) -> Option<Result<Vec<PathBuf>, String>> {
        self.result.borrow().clone()
    }
    pub async fn wait(&self) -> Result<Vec<PathBuf>, String> {
        loop {
            if let Some(r) = self.result() {
                return r;
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        }
    }
    pub fn cancel(&self) {
        if self.result.borrow().is_none() {
            self.progress.cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub fn drag_root() -> PathBuf {
    crate::config::cache_dir().join("drag")
}

/// Extract `sel` into a fresh folder under the drag cache. The files come out where they are
/// in the archive (…/<n>/proj/src/main.rs), so each selected path is just joined on.
pub fn start(arc: Arc<Opened>, sel: Vec<String>) -> Rc<Pending> {
    let p = Rc::new(Pending { progress: Arc::default(), result: RefCell::new(None) });
    let (prog, pw) = (p.progress.clone(), Rc::downgrade(&p));
    glib::spawn_future_local(async move {
        let r = gio::spawn_blocking(move || -> Result<Vec<PathBuf>, String> {
            let root = drag_root();
            std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
            let dir = backend::temp_dir_in(&root, "drag")?;
            let res = backend::extract_raw(&arc.path, &arc.listing, &sel, &dir, &prog).and_then(|()| {
                let files: Vec<PathBuf> = sel.iter().map(|s| dir.join(s)).collect();
                match files.iter().find(|f| f.symlink_metadata().is_err()) {
                    Some(f) => Err(format!("{} wasn't extracted", f.display())),
                    None => Ok(files),
                }
            });
            if res.is_err() {
                let _ = std::fs::remove_dir_all(&dir);
            }
            res
        })
        .await
        .unwrap_or_else(|_| Err("extract crashed".into()));
        if let Some(p) = pw.upgrade() {
            *p.result.borrow_mut() = Some(r);
        }
    });
    p
}

/// Remove drag folders older than a day (on a worker thread).
pub fn clean_old() {
    std::thread::spawn(|| {
        let Ok(rd) = std::fs::read_dir(drag_root()) else { return };
        let day = Duration::from_secs(24 * 3600);
        for e in rd.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > day);
            if old && e.file_name().to_string_lossy().contains("unleash-part") {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    });
}

fn plain_paths(files: &[PathBuf]) -> Vec<u8> {
    let mut s = String::new();
    for f in files {
        s.push_str(&f.to_string_lossy());
        s.push('\n');
    }
    s.into_bytes()
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LazyFiles {
        pub pending: RefCell<Option<Rc<Pending>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LazyFiles {
        const NAME: &'static str = "UnleashLazyFiles";
        type Type = super::LazyFiles;
        type ParentType = gdk::ContentProvider;
    }

    impl ObjectImpl for LazyFiles {}

    impl ContentProviderImpl for LazyFiles {
        fn formats(&self) -> gdk::ContentFormats {
            // the same mime types GTK offers for real files (uri-list + portal), plus plain text
            gdk::ContentFormats::for_type(gdk::FileList::static_type())
                .union_serialize_mime_types()
                .union(&gdk::ContentFormats::new(&["text/plain;charset=utf-8", "text/plain"]))
        }

        fn write_mime_type_future(
            &self,
            mime: &str,
            stream: &gio::OutputStream,
            prio: glib::Priority,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), glib::Error>> + 'static>> {
            let pending = self.pending.borrow().clone();
            let (mime, stream) = (mime.to_string(), stream.clone());
            Box::pin(async move {
                let fail = |m: &str| glib::Error::new(gio::IOErrorEnum::Failed, m);
                let pending = pending.ok_or_else(|| fail("nothing to drag"))?;
                let files = pending.wait().await.map_err(|e| fail(&e))?;
                if mime.starts_with("text/plain") {
                    stream.write_all_future(plain_paths(&files), prio).await.map_err(|(_, e)| e)?;
                    return stream.close_future(prio).await;
                }
                // real files now: GTK's own serializers do uri-list and the portal
                let list = gdk::FileList::from_array(&files.iter().map(gio::File::for_path).collect::<Vec<_>>());
                let inner = gdk::ContentProvider::for_value(&list.to_value());
                inner.write_mime_type_future(&mime, &stream, prio).await
            })
        }

        fn value(&self, t: glib::Type) -> Result<glib::Value, glib::Error> {
            let fail = || glib::Error::new(gio::IOErrorEnum::NotSupported, "not ready");
            if t != gdk::FileList::static_type() {
                return Err(fail());
            }
            let files = self.pending.borrow().as_ref().and_then(|p| p.result()).and_then(Result::ok).ok_or_else(fail)?;
            Ok(gdk::FileList::from_array(&files.iter().map(gio::File::for_path).collect::<Vec<_>>()).to_value())
        }
    }
}

glib::wrapper! {
    pub struct LazyFiles(ObjectSubclass<imp::LazyFiles>) @extends gdk::ContentProvider;
}

impl LazyFiles {
    pub fn new(p: Rc<Pending>) -> LazyFiles {
        let o: LazyFiles = glib::Object::new();
        *o.imp().pending.borrow_mut() = Some(p);
        o
    }
}
