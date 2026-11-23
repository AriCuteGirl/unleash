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
    /// once known (asked, or from the keyring) for as long as the archive is open
    pub password: std::sync::Mutex<Option<String>>,
}

impl Opened {
    pub fn new(path: PathBuf, listing: Listing, password: Option<String>) -> Opened {
        let tree = crate::tree::Tree::build(&listing.entries);
        Opened { path, listing, tree, password: std::sync::Mutex::new(password) }
    }
    pub fn password(&self) -> Option<String> {
        self.password.lock().ok().and_then(|p| p.clone())
    }
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
            let pw = arc.password();
            let res = backend::extract_raw_pw(&arc.path, &arc.listing, &sel, &dir, &prog, pw.as_deref()).and_then(|()| {
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
            // (the portal types are only there when the file transfer portal is running)
            gdk::ContentFormats::for_type(gdk::FileList::static_type())
                .union_serialize_mime_types()
                .union(&gdk::ContentFormats::new(&["text/uri-list", "text/plain;charset=utf-8", "text/plain"]))
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
                gdk::content_serialize_future(&stream, &mime, &list.to_value(), prio).await
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

#[cfg(test)]
mod tests {
    use super::*;

    fn read(p: &LazyFiles, mime: &str) -> String {
        let out = gio::MemoryOutputStream::new_resizable();
        let s: gio::OutputStream = out.clone().upcast();
        glib::MainContext::default().block_on(p.write_mime_type_future(mime, &s, glib::Priority::DEFAULT)).unwrap();
        let _ = s.close(gio::Cancellable::NONE);
        String::from_utf8(out.steal_as_bytes().to_vec()).unwrap()
    }

    #[test]
    fn offers_uri_list_portal_and_text_and_waits_for_files() {
        if gtk::init().is_err() {
            return; // no display to test with
        }
        let d = std::env::temp_dir().join(format!("unleash-drag-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a b.txt");
        std::fs::write(&f, "x").unwrap();
        let p = LazyFiles::new(Pending::ready(vec![f.clone()]));
        let mimes: Vec<String> = p.formats().mime_types().iter().map(|m| m.to_string()).collect();
        eprintln!("offered: {mimes:?}");
        assert!(mimes.iter().any(|m| m == "text/uri-list"));
        assert!(mimes.iter().any(|m| m.starts_with("text/plain")));
        let uri = read(&p, "text/uri-list");
        assert!(uri.starts_with("file://") && uri.contains("a%20b.txt"), "{uri}");
        if std::env::var_os("UNLEASH_TEST_PORTAL").is_some() {
            eprintln!("portal key: {:?}", read(&p, "application/vnd.portal.filetransfer"));
        }
        assert_eq!(read(&p, "text/plain;charset=utf-8"), format!("{}\n", f.display()));

        // not ready yet: the read waits until the files are there
        let pending = Rc::new(Pending { progress: Arc::default(), result: RefCell::new(None) });
        let lazy = LazyFiles::new(pending.clone());
        let pw = pending.clone();
        let f2 = f.clone();
        glib::timeout_add_local_once(Duration::from_millis(150), move || *pw.result.borrow_mut() = Some(Ok(vec![f2])));
        let t = std::time::Instant::now();
        assert!(read(&lazy, "text/uri-list").contains("a%20b.txt"));
        assert!(t.elapsed() >= Duration::from_millis(140));
    }
}
