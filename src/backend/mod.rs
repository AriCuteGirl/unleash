//! Reading archives. No GTK in here, so it all runs on worker threads and is unit-tested.
//!
//! The format is found from the file's first bytes, not its name:
//! - zip, tar, tar.gz, tar.zst, tar.xz and single .gz/.zst/.xz files: Rust crates, in process
//! - tar.bz2/lz4/lzma/Z: `bsdtar` turns them into a plain tar stream that the tar crate reads
//! - everything else (7z, rar, iso, cab, ...): the `7z` program
//!
//! Extracting always goes into a fresh temp folder; `place` moves the result into the real
//! destination under free names, so nothing is ever overwritten and a cancelled or failed
//! extract leaves nothing half-done behind.

pub mod create;
mod crates;
pub mod modify;
mod sevenz;

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Comp {
    None,
    Gz,
    Zst,
    Xz,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Zip,
    /// tar, optionally compressed with something we can decode in process
    Tar(Comp),
    /// one compressed file (notes.txt.gz)
    Single(Comp),
    /// tar compressed with something only bsdtar decodes (bz2, lz4, lzma, compress)
    BsdTar,
    SevenZ,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Zip => "ZIP",
            Kind::Tar(Comp::None) => "tar",
            Kind::Tar(Comp::Gz) => "tar.gz",
            Kind::Tar(Comp::Zst) => "tar.zst",
            Kind::Tar(Comp::Xz) => "tar.xz",
            Kind::Single(Comp::Gz) => "gzip",
            Kind::Single(Comp::Zst) => "zstd",
            Kind::Single(Comp::Xz) => "xz",
            Kind::Single(Comp::None) => "file",
            Kind::BsdTar => "tar",
            Kind::SevenZ => "7-Zip",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// '/'-separated, no leading or trailing slash
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub packed: Option<u64>,
    /// local time, "YYYY-MM-DD HH:MM" (sorts as text)
    pub modified: String,
    pub encrypted: bool,
    pub method: String,
}

#[derive(Debug)]
pub struct Listing {
    pub kind: Kind,
    /// what 7z says the type is ("Rar5", "Iso"), else the kind's label
    pub type_name: String,
    pub entries: Vec<Entry>,
}

/// Shared between a worker and the UI, which polls it.
#[derive(Default)]
pub struct Progress {
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
}

impl Progress {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
    pub fn set_current(&self, s: &str) {
        if let Ok(mut c) = self.current.lock() {
            s.clone_into(&mut c);
        }
    }
    pub fn add(&self, n: u64) {
        self.done.fetch_add(n, Ordering::Relaxed);
    }
    pub fn fraction(&self) -> f64 {
        let t = self.total.load(Ordering::Relaxed);
        if t == 0 { 0.0 } else { (self.done.load(Ordering::Relaxed) as f64 / t as f64).min(1.0) }
    }
}

pub const CANCELLED: &str = "Cancelled";

pub fn detect(path: &Path) -> Result<Kind, String> {
    let mut head = [0u8; 512];
    let mut f = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let n = read_full(&mut f, &mut head).map_err(|e| e.to_string())?;
    let h = &head[..n];
    let comp = if h.starts_with(&[0x1f, 0x8b]) {
        Some(Comp::Gz)
    } else if h.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(Comp::Zst)
    } else if h.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(Comp::Xz)
    } else {
        None
    };
    if let Some(c) = comp {
        // a tar inside, or one plain file?
        let mut r = crates::decoder(path, c)?;
        let mut inner = [0u8; 512];
        let n = read_full(&mut r, &mut inner).unwrap_or(0);
        return Ok(if is_tar(&inner[..n]) { Kind::Tar(c) } else { Kind::Single(c) });
    }
    if h.starts_with(b"PK\x03\x04") || h.starts_with(b"PK\x05\x06") || h.starts_with(b"PK\x07\x08") {
        return Ok(Kind::Zip);
    }
    if is_tar(h) {
        return Ok(Kind::Tar(Comp::None));
    }
    // bzip2, lz4, lzma-alone, unix compress: bsdtar when it's there (7z sees them as one file)
    let bsd = h.starts_with(b"BZh") || h.starts_with(&[0x04, 0x22, 0x4d, 0x18]) || h.starts_with(&[0x5d, 0, 0]) || h.starts_with(&[0x1f, 0x9d]);
    if bsd && have("bsdtar") {
        return Ok(Kind::BsdTar);
    }
    Ok(Kind::SevenZ)
}

fn is_tar(h: &[u8]) -> bool {
    h.len() >= 263 && &h[257..262] == b"ustar"
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

pub fn have(cmd: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(cmd).is_file()))
}

pub fn list(path: &Path) -> Result<Listing, String> {
    let kind = detect(path)?;
    let (kind, type_name, entries) = match kind {
        Kind::Zip => match crates::list_zip(path) {
            Ok(e) => (kind, kind.label().to_string(), e),
            // methods we don't build in (deflate64, bzip2, lzma, ...) or odd zips: 7z can do them
            Err(_) if have("7z") => {
                let (t, e) = sevenz::list(path)?;
                (Kind::SevenZ, t, e)
            }
            Err(e) => return Err(e),
        },
        Kind::Tar(c) => (kind, kind.label().to_string(), crates::list_tar(crates::decoder(path, c)?)?),
        Kind::BsdTar => (kind, "tar".to_string(), crates::list_tar(crates::bsdtar_stream(path)?)?),
        Kind::Single(c) => (kind, kind.label().to_string(), vec![crates::single_entry(path, c)?]),
        Kind::SevenZ => {
            if !have("7z") {
                return Err("This format needs 7-Zip (the 7zip package)".into());
            }
            let (t, e) = sevenz::list(path)?;
            (kind, t, e)
        }
    };
    Ok(Listing { kind, type_name, entries: normalize(entries) })
}

/// Clean paths, drop duplicates (a later entry wins, like extracting does), drop the empty name.
fn normalize(entries: Vec<Entry>) -> Vec<Entry> {
    let mut seen = HashSet::new();
    let mut out: Vec<Entry> = entries
        .into_iter()
        .rev()
        .filter_map(|mut e| {
            e.path = clean_path(&e.path)?;
            seen.insert(e.path.clone()).then_some(e)
        })
        .collect();
    out.reverse();
    out
}

/// "./a//b/" -> "a/b". None for empty names or anything that climbs out ("..", absolute).
pub fn clean_path(p: &str) -> Option<String> {
    let p = p.replace('\\', "/");
    let mut parts = vec![];
    for c in p.split('/') {
        match c {
            "" | "." => {}
            ".." => return None,
            c => parts.push(c),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Does `path` fall under one of the selected paths (the path itself or inside it)?
/// An empty selection means everything.
pub fn selected(path: &str, sel: &[String]) -> bool {
    sel.is_empty() || sel.iter().any(|s| path == s || (path.len() > s.len() && path.starts_with(s.as_str()) && path.as_bytes()[s.len()] == b'/'))
}

/// Extract `sel` (entry paths; empty = everything) of `archive` into the empty folder `into`,
/// keeping the paths from the archive.
pub fn extract_raw(archive: &Path, listing: &Listing, sel: &[String], into: &Path, p: &Progress) -> Result<(), String> {
    let total: u64 = listing.entries.iter().filter(|e| !e.is_dir && selected(&e.path, sel)).map(|e| e.size).sum();
    p.total.store(total.max(1), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    let r = match listing.kind {
        Kind::Zip => crates::extract_zip(archive, sel, into, p),
        Kind::Tar(c) => crates::extract_tar(crates::decoder(archive, c)?, sel, into, p),
        Kind::BsdTar => crates::extract_tar(crates::bsdtar_stream(archive)?, sel, into, p),
        Kind::Single(c) => crates::extract_single(archive, c, &listing.entries[0].path, into, p),
        Kind::SevenZ => sevenz::extract(archive, sel, into, p),
    };
    if p.cancelled() {
        return Err(CANCELLED.into());
    }
    r
}

/// A fresh hidden temp folder inside `dir` (same drive, so the final move is a rename).
pub fn temp_dir_in(dir: &Path, tag: &str) -> Result<PathBuf, String> {
    for n in 0..1000 {
        let d = dir.join(format!(".{tag}.unleash-part-{}-{n}", std::process::id()));
        match std::fs::create_dir(&d) {
            Ok(()) => return Ok(d),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Can't write to {}: {e}", dir.display())),
        }
    }
    Err("No free temp name".into())
}

/// A hidden, unused file name in `dir` for writing `name<ext>` before the final rename.
pub fn temp_name_in(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let base = format!(".{name}.unleash-part-{}", std::process::id());
    let n = (0..).find(|n| fs_free(&dir.join(format!("{base}-{n}{ext}")))).unwrap_or(0);
    dir.join(format!("{base}-{n}{ext}"))
}

fn fs_free(p: &Path) -> bool {
    p.symlink_metadata().is_err()
}

fn free_in(dir: &Path, name: &str) -> PathBuf {
    dir.join(crate::util::free_name(name, |n| dir.join(n).symlink_metadata().is_ok()))
}

/// "photos.tar.gz" -> "photos", "notes.txt.gz" -> "notes.txt", "x.7z.001" -> "x"
pub fn stem(name: &str) -> String {
    const EXTS: &[&str] = &[
        ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tar.lz4", ".tar.lzma", ".tar.z", ".tgz", ".txz", ".tbz2", ".tbz", ".tzst", ".tar", ".zip",
        ".7z", ".rar", ".gz", ".xz", ".zst", ".bz2", ".iso", ".cab", ".cbz", ".cbr", ".jar", ".apk", ".deb", ".rpm", ".cpio", ".wim", ".lzh", ".arj",
    ];
    let mut s = name.to_string();
    // split volumes: name.7z.001, name.zip.001, name.part1.rar
    if let Some(i) = s.rfind('.') {
        if s[i + 1..].len() == 3 && s[i + 1..].bytes().all(|b| b.is_ascii_digit()) {
            s.truncate(i);
        }
    }
    let l = s.to_lowercase();
    if let Some(e) = EXTS.iter().find(|e| l.ends_with(*e) && l.len() > e.len()) {
        s.truncate(s.len() - e.len());
        let l = s.to_lowercase();
        if let Some(i) = l.rfind(".part").filter(|&i| l[i + 5..].bytes().all(|b| b.is_ascii_digit()) && l.len() > i + 5) {
            s.truncate(i);
        }
    }
    s
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Placement {
    /// one thing at the top: that thing; several: a folder named after the archive
    Smart,
    /// each item of the extracted folder goes straight into the destination
    Loose,
}

/// Extract `sel` (all under `base`, the folder being browsed; "" = root) into `dest`.
/// Returns what was created there.
pub fn extract_to(archive: &Path, listing: &Listing, sel: &[String], base: &str, dest: &Path, how: Placement, p: &Progress) -> Result<Vec<PathBuf>, String> {
    let tmp = temp_dir_in(dest, "extract")?;
    let res = extract_raw(archive, listing, sel, &tmp, p).and_then(|()| {
        let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        place(&tmp, base, dest, how, &stem(&name))
    });
    let _ = std::fs::remove_dir_all(&tmp);
    res
}

/// Move the extracted items from `tmp/base` into `dest` under free names.
fn place(tmp: &Path, base: &str, dest: &Path, how: Placement, wrap_name: &str) -> Result<Vec<PathBuf>, String> {
    let root = if base.is_empty() { tmp.to_path_buf() } else { tmp.join(base) };
    let items: Vec<PathBuf> = match std::fs::read_dir(&root) {
        Ok(rd) => rd.flatten().map(|e| e.path()).collect(),
        Err(_) => vec![],
    };
    if items.is_empty() {
        return Err("Nothing was extracted".into());
    }
    let mv = |from: &Path, to: PathBuf| std::fs::rename(from, &to).map(|()| to).map_err(|e| e.to_string());
    if how == Placement::Smart && items.len() > 1 {
        let target = free_in(dest, if wrap_name.is_empty() { "extracted" } else { wrap_name });
        return Ok(vec![mv(&root, target)?]);
    }
    items
        .iter()
        .map(|it| {
            let n = it.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            mv(it, free_in(dest, &n))
        })
        .collect()
}

/// Copy with progress and cancel checks between chunks.
pub(crate) fn copy_counted(r: &mut impl Read, w: &mut impl std::io::Write, p: &Progress) -> Result<(), String> {
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let n = match r.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        };
        w.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        p.add(n as u64);
    }
}

/// Civil date from unix seconds in local time (for tar), "YYYY-MM-DD HH:MM".
pub(crate) fn local_time(secs: i64) -> String {
    gtk::glib::DateTime::from_unix_local(secs)
        .and_then(|d| d.format("%Y-%m-%d %H:%M"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
