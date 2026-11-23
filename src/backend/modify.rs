//! Changing an archive: adding files and deleting entries.
//!
//! Never in place. A new archive is written next to the old one under a hidden temp name, read
//! back to check it has what it should, and only then renamed over the original (an atomic
//! swap on the same drive). A failure or cancel at any point leaves the original untouched.
//!
//! - zip: kept entries are copied raw (no recompression), new files deflated
//! - tar, tar.gz/xz/zst: re-streamed into the same compression
//! - 7z and zip that 7z handles: 7z works on a copy of the archive
//! - everything else (rar, iso, tar.bz2, single .gz, ...) can't be changed: extract and make a
//!   new one

use super::create::{self, Level};
use super::{CANCELLED, Comp, Kind, Listing, Progress, clean_path, have, selected, sevenz, temp_name_in};
use std::fs::{self, File};
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

/// Can this archive be changed? Err says why not, in words for the user.
pub fn can_modify(l: &Listing) -> Result<(), String> {
    match l.kind {
        _ if l.entries.iter().any(|e| e.encrypted) => Err("Archives with a password can't be changed yet".into()),
        Kind::Zip | Kind::Tar(_) => Ok(()),
        Kind::SevenZ if matches!(l.type_name.as_str(), "7z" | "zip") && have("7z") => Ok(()),
        Kind::SevenZ if l.entries.iter().any(|e| e.encrypted) => Err("Encrypted archives can't be changed yet".into()),
        _ => Err(format!("{} archives can't be changed. Extract it and make a new one.", l.type_name)),
    }
}

/// Paths inside the archive that adding `items` under `base` would replace.
pub fn clashes(l: &Listing, items: &[PathBuf], base: &str) -> Vec<String> {
    let new: Vec<String> = items.iter().filter_map(|i| i.file_name()).map(|n| join(base, &n.to_string_lossy())).collect();
    let mut v: Vec<String> = new.into_iter().filter(|n| l.entries.iter().any(|e| e.path == *n || selected(&e.path, std::slice::from_ref(n)))).collect();
    v.dedup();
    v
}

fn join(base: &str, name: &str) -> String {
    if base.is_empty() { name.to_string() } else { format!("{base}/{name}") }
}

/// Add files and folders into the folder `base` of the archive. Same names are replaced.
pub fn add(archive: &Path, l: &Listing, items: &[PathBuf], base: &str, p: &Progress) -> Result<(), String> {
    can_modify(l)?;
    let mut files = create::walk(items)?;
    for f in &mut files {
        f.1 = join(base, &f.1);
    }
    let replaced: Vec<String> = clashes(l, items, base);
    // progress: the kept entries get copied too
    let kept: u64 = l.entries.iter().filter(|e| replaced.is_empty() || !selected(&e.path, &replaced)).map(|e| e.size).sum();
    let new: u64 = files.iter().filter(|f| f.2.is_file()).map(|f| f.2.len()).sum();
    p.total.store((kept + new).max(1), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    let expect_in: Vec<String> = files.iter().map(|f| f.1.clone()).collect();
    swap(archive, p, &expect_in, &[], |tmp| match l.kind {
        Kind::Zip => zip_rewrite(archive, tmp, &replaced, &files, p),
        Kind::Tar(c) => tar_rewrite(archive, c, tmp, &replaced, &files, p),
        _ => {
            fs::copy(archive, tmp).map_err(|e| e.to_string())?;
            if !replaced.is_empty() {
                sevenz::delete(tmp, &replaced)?;
            }
            seven_add(tmp, items, base, p)
        }
    })
}

/// Delete entries (folders with everything in them).
pub fn delete(archive: &Path, l: &Listing, paths: &[String], p: &Progress) -> Result<(), String> {
    can_modify(l)?;
    if paths.is_empty() {
        return Ok(());
    }
    p.total.store(l.entries.iter().filter(|e| !selected(&e.path, paths)).map(|e| e.size).sum::<u64>().max(1), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    swap(archive, p, &[], paths, |tmp| match l.kind {
        Kind::Zip => zip_rewrite(archive, tmp, paths, &[], p),
        Kind::Tar(c) => tar_rewrite(archive, c, tmp, paths, &[], p),
        _ => {
            fs::copy(archive, tmp).map_err(|e| e.to_string())?;
            sevenz::delete(tmp, paths)
        }
    })
}

/// Write the new archive with `write`, check it, then rename it over the old one.
fn swap(archive: &Path, p: &Progress, must_have: &[String], must_not: &[String], write: impl FnOnce(&Path) -> Result<(), String>) -> Result<(), String> {
    let dir = archive.parent().ok_or("No folder")?;
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    // the temp keeps the archive's extension: 7z picks the format from it
    let ext = name.find('.').map(|i| name[i..].to_string()).unwrap_or_default();
    let tmp = temp_name_in(dir, "update", &ext);
    let res = write(&tmp).and_then(|()| {
        if p.cancelled() {
            return Err(CANCELLED.to_string());
        }
        let check = super::list(&tmp).map_err(|e| format!("The new archive didn't read back ({e}); the original is unchanged"))?;
        let has = |x: &str| check.entries.iter().any(|e| e.path == x);
        if let Some(m) = must_have.iter().find(|x| !has(x)) {
            return Err(format!("{m} is missing from the new archive; the original is unchanged"));
        }
        if check.entries.iter().any(|e| selected(&e.path, must_not) && !must_not.is_empty()) {
            return Err("Something wasn't deleted; the original is unchanged".into());
        }
        if let Ok(md) = fs::metadata(archive) {
            let _ = fs::set_permissions(&tmp, md.permissions());
        }
        fs::rename(&tmp, archive).map_err(|e| e.to_string())
    });
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    if p.cancelled() { Err(CANCELLED.into()) } else { res }
}

fn zip_rewrite(archive: &Path, tmp: &Path, drop: &[String], add: &[(PathBuf, String, fs::Metadata)], p: &Progress) -> Result<(), String> {
    let mut old = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(std::io::BufWriter::new(File::create_new(tmp).map_err(|e| e.to_string())?));
    for i in 0..old.len() {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let e = old.by_index_raw(i).map_err(|e| e.to_string())?;
        let keep = clean_path(e.name()).is_some_and(|n| drop.is_empty() || !selected(&n, drop));
        if keep {
            p.set_current(e.name());
            let size = e.size();
            z.raw_copy_file(e).map_err(|e| e.to_string())?;
            p.add(size);
        }
    }
    create::zip_entries(&mut z, add, Level::Normal, p, None)?;
    create::zip_finish(z)
}

fn tar_rewrite(archive: &Path, comp: Comp, tmp: &Path, drop: &[String], add: &[(PathBuf, String, fs::Metadata)], p: &Progress) -> Result<(), String> {
    let mut b = create::tar_builder(create::open_enc(tmp, comp, Level::Normal)?, p);
    let mut old = tar::Archive::new(super::crates::decoder(archive, comp)?);
    for e in old.entries().map_err(|e| e.to_string())? {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let mut e = e.map_err(|e| e.to_string())?;
        let raw = String::from_utf8_lossy(&e.path_bytes()).to_string();
        let Some(rel) = clean_path(&raw) else { continue };
        if !drop.is_empty() && selected(&rel, drop) {
            continue;
        }
        p.set_current(&rel);
        let mut h = e.header().clone();
        let kind = h.entry_type();
        let path = e.path().map_err(|e| e.to_string())?.into_owned();
        let r = if kind.is_symlink() || kind.is_hard_link() {
            let target = e.link_name().map_err(|e| e.to_string())?.map(|t| t.into_owned()).unwrap_or_default();
            b.append_link(&mut h, &path, &target)
        } else {
            b.append_data(&mut h, &path, &mut e) // (the Counted writer moves the progress)
        };
        r.map_err(|err| format!("{rel}: {}", create::io_err(err, p)))?;
    }
    create::tar_entries(&mut b, add, p)?;
    create::tar_finish(b, p)
}

/// 7z can't put files into a folder of the archive directly: stage links under `base` in an
/// empty folder and add from there (7z follows the links and stores the real files).
fn seven_add(archive: &Path, items: &[PathBuf], base: &str, p: &Progress) -> Result<(), String> {
    if base.is_empty() {
        let mut ok = Ok(());
        for (dir, names) in group_by_dir(items) {
            ok = ok.and_then(|()| sevenz::update(&dir, &names, archive, false, p));
        }
        return ok;
    }
    let stage = super::temp_dir_in(archive.parent().ok_or("No folder")?, "stage")?;
    let res = (|| {
        fs::create_dir_all(stage.join(base)).map_err(|e| e.to_string())?;
        for it in items {
            let n = it.file_name().ok_or("no name")?;
            let abs = std::path::absolute(it).map_err(|e| e.to_string())?;
            std::os::unix::fs::symlink(&abs, stage.join(base).join(n)).map_err(|e| e.to_string())?;
        }
        let top = base.split('/').next().unwrap_or(base).to_string();
        sevenz::update(&stage, &[top], archive, true, p)
    })();
    let _ = fs::remove_dir_all(&stage);
    res
}

fn group_by_dir(items: &[PathBuf]) -> Vec<(PathBuf, Vec<String>)> {
    let mut v: Vec<(PathBuf, Vec<String>)> = vec![];
    for it in items {
        let dir = it.parent().unwrap_or(Path::new("/")).to_path_buf();
        let n = it.file_name().unwrap_or_default().to_string_lossy().to_string();
        match v.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, names)) => names.push(n),
            None => v.push((dir, vec![n])),
        }
    }
    v
}
