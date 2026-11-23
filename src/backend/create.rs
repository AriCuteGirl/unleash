//! Making archives. Written to a hidden temp name next to the result and renamed when complete,
//! so a cancelled or failed compress leaves nothing behind and nothing is ever overwritten.

use super::{CANCELLED, Progress, copy_counted, have, sevenz, temp_name_in};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Zip,
    SevenZ,
    TarGz,
    TarXz,
    TarZst,
}

impl Format {
    #[allow(dead_code)] // for the Create dialog
    pub const ALL: [Format; 5] = [Format::Zip, Format::SevenZ, Format::TarGz, Format::TarXz, Format::TarZst];

    pub fn ext(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::SevenZ => ".7z",
            Format::TarGz => ".tar.gz",
            Format::TarXz => ".tar.xz",
            Format::TarZst => ".tar.zst",
        }
    }

    /// "zip", "7z", "tar.gz", "tgz", ".tar.zst", ...
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s.trim().trim_start_matches('.').to_lowercase().as_str() {
            "zip" => Format::Zip,
            "7z" | "7zip" => Format::SevenZ,
            "tar.gz" | "tgz" | "gz" => Format::TarGz,
            "tar.xz" | "txz" | "xz" => Format::TarXz,
            "tar.zst" | "tzst" | "zst" | "zstd" => Format::TarZst,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Level {
    Fast,
    Normal,
    Best,
}

/// Every file under `items` with its size (symlinks are stored as links, not followed).
fn walk(items: &[PathBuf]) -> Result<Vec<(PathBuf, String, fs::Metadata)>, String> {
    let mut out = vec![];
    let mut stack: Vec<(PathBuf, String)> = vec![];
    for it in items.iter().rev() {
        let name = it.file_name().ok_or_else(|| format!("{} has no name", it.display()))?.to_string_lossy().to_string();
        stack.push((it.clone(), name));
    }
    while let Some((p, rel)) = stack.pop() {
        let md = fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if md.is_dir() {
            let mut kids: Vec<_> = fs::read_dir(&p).map_err(|e| format!("{}: {e}", p.display()))?.flatten().map(|e| e.path()).collect();
            kids.sort();
            for k in kids.into_iter().rev() {
                let n = k.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                stack.push((k, format!("{rel}/{n}")));
            }
        }
        out.push((p, rel, md));
    }
    Ok(out)
}

/// Pack `items` into `<dest>/<name><ext>` (a free name). Returns the archive.
pub fn create(items: &[PathBuf], dest: &Path, name: &str, fmt: Format, level: Level, p: &Progress) -> Result<PathBuf, String> {
    if items.is_empty() {
        return Err("Nothing to compress".into());
    }
    // the same name twice at the top would collide inside the archive
    let mut tops: Vec<_> = items.iter().filter_map(|i| i.file_name()).collect();
    tops.sort();
    if tops.windows(2).any(|w| w[0] == w[1]) {
        return Err("Two of the items have the same name".into());
    }
    let files = walk(items)?;
    p.total.store(files.iter().filter(|f| f.2.is_file()).map(|f| f.2.len()).sum::<u64>().max(1), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    let name = if name.trim().is_empty() { "archive" } else { name.trim() };
    let tmp = temp_name_in(dest, name, fmt.ext());
    let res = match fmt {
        Format::Zip => zip(&files, &tmp, level, p),
        Format::SevenZ => {
            if !have("7z") {
                Err("7z archives need 7-Zip (the 7zip package)".into())
            } else {
                seven(items, &tmp, level, p)
            }
        }
        Format::TarGz | Format::TarXz | Format::TarZst => tar(&files, &tmp, fmt, level, p),
    };
    // a cancel shows up as an error from whatever was writing; report it as just that
    let res = if p.cancelled() { Err(CANCELLED.to_string()) } else { res };
    let res = res.and_then(|()| {
        let target = super::free_in(dest, &format!("{name}{}", fmt.ext()));
        fs::rename(&tmp, &target).map_err(|e| e.to_string())?;
        Ok(target)
    });
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

fn seven(items: &[PathBuf], out: &Path, level: Level, p: &Progress) -> Result<(), String> {
    // 7z gets names relative to one folder; items from several folders go in one by one
    let lvl = match level {
        Level::Fast => 1,
        Level::Normal => 5,
        Level::Best => 9,
    };
    let mut by_dir: Vec<(PathBuf, Vec<String>)> = vec![];
    for it in items {
        let dir = it.parent().unwrap_or(Path::new("/")).to_path_buf();
        let n = it.file_name().unwrap_or_default().to_string_lossy().to_string();
        match by_dir.iter_mut().find(|(d, _)| *d == dir) {
            Some((_, v)) => v.push(n),
            None => by_dir.push((dir, vec![n])),
        }
    }
    let out = std::path::absolute(out).map_err(|e| e.to_string())?;
    for (dir, names) in by_dir {
        sevenz::create(&dir, &names, &out, lvl, p)?;
    }
    Ok(())
}

fn zip(files: &[(PathBuf, String, fs::Metadata)], out: &Path, level: Level, p: &Progress) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let f = File::create_new(out).map_err(|e| e.to_string())?;
    let mut z = zip::ZipWriter::new(BufWriter::new(f));
    let lvl = match level {
        Level::Fast => 1,
        Level::Normal => 6,
        Level::Best => 9,
    };
    for (path, rel, md) in files {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        p.set_current(rel);
        let mtime = md.modified().ok().and_then(|t| zip_time(t));
        let mut o = zip::write::SimpleFileOptions::default().unix_permissions(md.permissions().mode() & 0o7777);
        if let Some(t) = mtime {
            o = o.last_modified_time(t);
        }
        let e = |e: zip::result::ZipError| format!("{rel}: {e}");
        if md.is_dir() {
            z.add_directory(format!("{rel}/"), o).map_err(e)?;
        } else if md.file_type().is_symlink() {
            let target = fs::read_link(path).map_err(|e| e.to_string())?;
            z.add_symlink(rel, target.to_string_lossy(), o).map_err(e)?;
        } else if md.is_file() {
            // already-compressed files are stored: deflate would only cost time
            // (a level is only valid for deflate: stored files must not get one)
            let o = if is_compressed(rel) {
                o.compression_method(zip::CompressionMethod::Stored)
            } else {
                o.compression_method(zip::CompressionMethod::Deflated).compression_level(Some(lvl))
            };
            let o = o.large_file(md.len() >= 0xFFFF_FFFF);
            z.start_file(rel, o).map_err(e)?;
            let mut r = File::open(path).map_err(|e| format!("{rel}: {e}"))?;
            copy_counted(&mut r, &mut z, p).map_err(|e| format!("{rel}: {e}"))?;
        }
    }
    let mut w = z.finish().map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    w.get_ref().sync_all().map_err(|e| e.to_string())
}

fn zip_time(t: std::time::SystemTime) -> Option<zip::DateTime> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    let d = gtk::glib::DateTime::from_unix_local(secs).ok()?;
    zip::DateTime::from_date_and_time(d.year() as u16, d.month() as u8, d.day_of_month() as u8, d.hour() as u8, d.minute() as u8, d.second() as u8).ok()
}

fn is_compressed(name: &str) -> bool {
    const EXT: &[&str] = &[
        ".jpg", ".jpeg", ".png", ".webp", ".gif", ".avif", ".heic", ".mp3", ".ogg", ".opus", ".flac", ".m4a", ".aac", ".mp4", ".mkv", ".webm", ".mov", ".avi",
        ".zip", ".7z", ".rar", ".gz", ".xz", ".zst", ".bz2", ".jar", ".apk", ".docx", ".xlsx", ".pptx", ".odt", ".pdf", ".epub",
    ];
    let l = name.to_lowercase();
    EXT.iter().any(|e| l.ends_with(e))
}

/// Counts what goes into the compressor and stops on cancel.
struct Counted<'a, W: Write> {
    w: W,
    p: &'a Progress,
}
impl<W: Write> Write for Counted<'_, W> {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.p.cancelled() {
            return Err(std::io::Error::other(CANCELLED));
        }
        let n = self.w.write(b)?;
        self.p.add(n as u64);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

enum Enc {
    Gz(flate2::write::GzEncoder<BufWriter<File>>),
    Xz(liblzma::write::XzEncoder<BufWriter<File>>),
    Zst(zstd::stream::write::Encoder<'static, BufWriter<File>>),
}
impl Write for Enc {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        match self {
            Enc::Gz(e) => e.write(b),
            Enc::Xz(e) => e.write(b),
            Enc::Zst(e) => e.write(b),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Enc::Gz(e) => e.flush(),
            Enc::Xz(e) => e.flush(),
            Enc::Zst(e) => e.flush(),
        }
    }
}
impl Enc {
    /// Write the compressor's tail and get the file back (errors here, e.g. disk full, count).
    fn finish(self) -> std::io::Result<File> {
        let w = match self {
            Enc::Gz(e) => e.finish()?,
            Enc::Xz(e) => e.finish()?,
            Enc::Zst(e) => e.finish()?,
        };
        w.into_inner().map_err(|e| e.into_error())
    }
}

fn tar(files: &[(PathBuf, String, fs::Metadata)], out: &Path, fmt: Format, level: Level, p: &Progress) -> Result<(), String> {
    let f = BufWriter::with_capacity(1 << 20, File::create_new(out).map_err(|e| e.to_string())?);
    let err = |e: std::io::Error| if p.cancelled() { CANCELLED.to_string() } else { e.to_string() };
    let pick = |fast: u32, normal: u32, best: u32| match level {
        Level::Fast => fast,
        Level::Normal => normal,
        Level::Best => best,
    };
    let enc = match fmt {
        Format::TarGz => Enc::Gz(flate2::write::GzEncoder::new(f, flate2::Compression::new(pick(1, 6, 9)))),
        Format::TarXz => Enc::Xz(liblzma::write::XzEncoder::new(f, pick(1, 6, 9))),
        _ => Enc::Zst(zstd::stream::write::Encoder::new(f, pick(1, 6, 15) as i32).map_err(err)?),
    };
    // progress counts tar bytes (headers are tiny next to the data)
    let mut b = tar::Builder::new(Counted { w: enc, p });
    b.follow_symlinks(false);
    b.mode(tar::HeaderMode::Complete);
    for (path, rel, _) in files {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        p.set_current(rel);
        b.append_path_with_name(path, rel).map_err(|e| format!("{rel}: {}", err(e)))?;
    }
    let c = b.into_inner().map_err(err)?;
    c.w.finish().and_then(|f| f.sync_all()).map_err(err)
}
