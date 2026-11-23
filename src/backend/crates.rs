//! zip and the tar family, in process.

use super::{CANCELLED, Comp, Entry, Progress, clean_path, copy_counted, local_time, selected};
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Component, Path};
use std::process::{Command, Stdio};

pub fn decoder(path: &Path, c: Comp) -> Result<Box<dyn Read + Send>, String> {
    let f = BufReader::with_capacity(256 * 1024, File::open(path).map_err(|e| e.to_string())?);
    Ok(match c {
        Comp::None => Box::new(f),
        Comp::Gz => Box::new(flate2::bufread::MultiGzDecoder::new(f)),
        Comp::Zst => Box::new(zstd::stream::read::Decoder::with_buffer(f).map_err(|e| e.to_string())?),
        Comp::Xz => Box::new(liblzma::bufread::XzDecoder::new_multi_decoder(f)),
    })
}

/// bsdtar re-packs anything libarchive reads as a plain tar on stdout.
pub fn bsdtar_stream(path: &Path) -> Result<Box<dyn Read + Send>, String> {
    let mut at = std::ffi::OsString::from("@");
    at.push(path.as_os_str());
    let mut child = Command::new("bsdtar")
        .args(["-cf", "-", "--format", "pax"])
        .arg(at)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start bsdtar: {e}"))?;
    let out = child.stdout.take().ok_or("bsdtar gave no output")?;
    // reap it when the reader is dropped
    struct Reaped(std::process::ChildStdout, std::process::Child);
    impl Read for Reaped {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(b)
        }
    }
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.1.kill();
            let _ = self.1.wait();
        }
    }
    Ok(Box::new(Reaped(out, child)))
}

// ------------------------------------------------------------------ zip

fn zip_time(d: Option<zip::DateTime>) -> String {
    d.map(|d| format!("{:04}-{:02}-{:02} {:02}:{:02}", d.year(), d.month(), d.day(), d.hour(), d.minute())).unwrap_or_default()
}

pub fn list_zip(path: &Path) -> Result<Vec<Entry>, String> {
    let f = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(z.len());
    for i in 0..z.len() {
        let e = z.by_index_raw(i).map_err(|e| e.to_string())?;
        let method = format!("{:?}", e.compression());
        // a method this build can't decode: let 7z handle the whole archive
        if !matches!(e.compression(), zip::CompressionMethod::Stored | zip::CompressionMethod::Deflated) {
            return Err(format!("unsupported zip method {method}"));
        }
        out.push(Entry {
            path: e.name().to_string(),
            is_dir: e.is_dir(),
            size: e.size(),
            packed: Some(e.compressed_size()),
            modified: zip_time(e.last_modified()),
            encrypted: e.encrypted(),
            method,
        });
    }
    Ok(out)
}

pub fn extract_zip(path: &Path, sel: &[String], into: &Path, p: &Progress) -> Result<(), String> {
    let f = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut z = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    for i in 0..z.len() {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let mut e = z.by_index(i).map_err(|e| match e {
            zip::result::ZipError::UnsupportedArchive(m) if m.contains("assword") => "This archive needs a password (coming in a later version)".to_string(),
            e => e.to_string(),
        })?;
        let Some(rel) = clean_path(e.name()) else { continue };
        if !selected(&rel, sel) {
            continue;
        }
        let out = into.join(&rel);
        p.set_current(&rel);
        if e.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        if e.is_symlink() {
            let mut target = String::new();
            e.read_to_string(&mut target).map_err(|e| e.to_string())?;
            safe_symlink(&target, &rel, &out)?;
            continue;
        }
        let mut w = File::create_new(&out).map_err(|e| format!("{rel}: {e}"))?;
        copy_counted(&mut e, &mut w, p).map_err(|e| format!("{rel}: {e}"))?;
        drop(w);
        if let Some(mode) = e.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&out, fs::Permissions::from_mode(mode & 0o777));
        }
    }
    Ok(())
}

/// Only links that stay inside the extracted tree.
fn safe_symlink(target: &str, rel: &str, out: &Path) -> Result<(), String> {
    let t = Path::new(target);
    let depth = rel.matches('/').count() as i64;
    let mut level = depth;
    for c in t.components() {
        match c {
            Component::ParentDir => level -= 1,
            Component::Normal(_) => level += 1,
            Component::CurDir => {}
            _ => return Ok(()), // absolute: skipped
        }
        if level < 0 {
            return Ok(()); // climbs out: skipped
        }
    }
    std::os::unix::fs::symlink(t, out).map_err(|e| e.to_string())
}

// ------------------------------------------------------------------ tar

pub fn list_tar(r: Box<dyn Read + Send>) -> Result<Vec<Entry>, String> {
    let mut a = tar::Archive::new(r);
    let mut out = vec![];
    for e in a.entries().map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let h = e.header();
        let kind = h.entry_type();
        if !(kind.is_file() || kind.is_dir() || kind.is_symlink() || kind.is_hard_link()) {
            continue;
        }
        let path = String::from_utf8_lossy(&e.path_bytes()).to_string();
        out.push(Entry {
            path,
            is_dir: kind.is_dir(),
            size: if kind.is_file() { e.size() } else { 0 },
            packed: None,
            modified: h.mtime().map(|t| local_time(t as i64)).unwrap_or_default(),
            encrypted: false,
            method: String::new(),
        });
    }
    Ok(out)
}

pub fn extract_tar(r: Box<dyn Read + Send>, sel: &[String], into: &Path, p: &Progress) -> Result<(), String> {
    let mut a = tar::Archive::new(r);
    a.set_preserve_permissions(true);
    a.set_preserve_mtime(true);
    a.set_overwrite(true);
    for e in a.entries().map_err(|e| e.to_string())? {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let mut e = e.map_err(|e| e.to_string())?;
        let raw = String::from_utf8_lossy(&e.path_bytes()).to_string();
        let Some(rel) = clean_path(&raw) else { continue };
        if !selected(&rel, sel) {
            continue;
        }
        p.set_current(&rel);
        let size = if e.header().entry_type().is_file() { e.size() } else { 0 };
        // unpack_in refuses ".." and writing through links that point outside `into`
        e.unpack_in(into).map_err(|err| format!("{rel}: {err}"))?;
        p.add(size);
    }
    Ok(())
}

// ------------------------------------------------------------------ one compressed file

pub fn single_entry(path: &Path, c: Comp) -> Result<Entry, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let inner = super::stem(&name);
    let inner = if inner == name { format!("{name}.out") } else { inner };
    let md = fs::metadata(path).map_err(|e| e.to_string())?;
    // the real size is only known by decoding (gzip's footer is mod 4 GiB)
    let mut r = decoder(path, c)?;
    let mut sink = std::io::sink();
    let size = std::io::copy(&mut r, &mut sink).map_err(|e| e.to_string())?;
    let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| local_time(d.as_secs() as i64)).unwrap_or_default();
    Ok(Entry { path: inner, is_dir: false, size, packed: Some(md.len()), modified: mtime, encrypted: false, method: String::new() })
}

pub fn extract_single(path: &Path, c: Comp, name: &str, into: &Path, p: &Progress) -> Result<(), String> {
    let mut r = decoder(path, c)?;
    p.set_current(name);
    let mut w = File::create_new(into.join(name)).map_err(|e| e.to_string())?;
    copy_counted(&mut r, &mut w, p)
}
