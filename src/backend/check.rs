//! "Test archive": read every file through to the end so each format's own checks run (zip and
//! 7z CRCs, gzip CRC, xz/zstd checksums, tar header sums). Nothing is written to disk.

use super::{CANCELLED, Comp, Kind, Listing, NEEDS_PASSWORD, Progress, WRONG_PASSWORD, clean_path, copy_counted, crates, sevenz};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    pub files_ok: u64,
    /// (path, what's wrong)
    pub bad: Vec<(String, String)>,
    /// set when the format can't prove the files are intact (plain tar, zstd without checksum)
    pub weak: Option<String>,
}

pub fn test(archive: &Path, l: &Listing, pw: Option<&str>, p: &Progress) -> Result<Report, String> {
    if pw.is_none() && super::needs_password(l, &[]) {
        return Err(NEEDS_PASSWORD.into());
    }
    let files = l.entries.iter().filter(|e| !e.is_dir).count() as u64;
    p.total.store(l.entries.iter().filter(|e| !e.is_dir).map(|e| e.size).sum::<u64>().max(1), Ordering::Relaxed);
    p.done.store(0, Ordering::Relaxed);
    let bad = match l.kind {
        Kind::Zip => zip(archive, pw, p)?,
        Kind::Tar(c) => tar(crates::decoder(archive, c)?, p)?,
        Kind::BsdTar => tar(crates::bsdtar_stream(archive)?, p)?,
        Kind::Single(c) => single(archive, c, &l.entries[0].path, p)?,
        Kind::SevenZ => sevenz::test(archive, p, pw)?,
    };
    if p.cancelled() {
        return Err(CANCELLED.into());
    }
    // a bad name may stand for a folder or for the whole archive: count what we can
    let files_ok = files.saturating_sub(bad.iter().filter(|(path, _)| l.entries.iter().any(|e| e.path == *path && !e.is_dir)).count() as u64);
    let weak = match l.kind {
        Kind::Tar(Comp::None) => Some("Plain tar has no checksums for file contents, so damage inside files can't be found.".to_string()),
        Kind::Tar(Comp::Zst) | Kind::Single(Comp::Zst) if !zstd_has_checksum(archive) => {
            Some("This zstd file was made without checksums, so damage inside files can't be found.".to_string())
        }
        _ => None,
    };
    Ok(Report { files_ok, bad, weak })
}

/// The first zstd frame's Content_Checksum_flag.
fn zstd_has_checksum(archive: &Path) -> bool {
    let mut h = [0u8; 5];
    File::open(archive).and_then(|mut f| f.read_exact(&mut h)).is_ok() && h[4] & 0b100 != 0
}

/// Sinks the data, counting it.
struct Sink;
impl std::io::Write for Sink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn zip(archive: &Path, pw: Option<&str>, p: &Progress) -> Result<Vec<(String, String)>, String> {
    let mut z = zip::ZipArchive::new(BufReader::new(File::open(archive).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
    let mut bad = vec![];
    for i in 0..z.len() {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let (name, enc, dir) = {
            let r = z.by_index_raw(i).map_err(|e| e.to_string())?;
            (r.name().to_string(), r.encrypted(), r.is_dir())
        };
        if dir {
            continue;
        }
        p.set_current(&name);
        let got = match (enc, pw) {
            (true, Some(pw)) => z.by_index_decrypt(i, pw.as_bytes()),
            _ => z.by_index(i),
        };
        let mut f = match got {
            Ok(f) => f,
            Err(zip::result::ZipError::InvalidPassword) => return Err(WRONG_PASSWORD.into()),
            Err(e) => {
                bad.push((clean_path(&name).unwrap_or(name), e.to_string()));
                continue;
            }
        };
        if let Err(e) = copy_counted(&mut f, &mut Sink, p) {
            if e == CANCELLED {
                return Err(e);
            }
            bad.push((clean_path(&name).unwrap_or(name), nice(&e)));
        }
    }
    Ok(bad)
}

fn tar(r: Box<dyn Read + Send>, p: &Progress) -> Result<Vec<(String, String)>, String> {
    let mut a = tar::Archive::new(r);
    let mut bad = vec![];
    let entries = match a.entries() {
        Ok(e) => e,
        Err(e) => return Ok(vec![(String::new(), nice(&e.to_string()))]),
    };
    let mut last = String::new();
    for e in entries {
        if p.cancelled() {
            return Err(CANCELLED.into());
        }
        let mut e = match e {
            Ok(e) => e,
            Err(err) => {
                // the stream itself broke: nothing after this point can be read
                bad.push((if last.is_empty() { String::new() } else { format!("after {last}") }, nice(&err.to_string())));
                break;
            }
        };
        let name = String::from_utf8_lossy(&e.path_bytes()).to_string();
        let name = clean_path(&name).unwrap_or(name);
        p.set_current(&name);
        if let Err(err) = copy_counted(&mut e, &mut Sink, p) {
            if err == CANCELLED {
                return Err(err);
            }
            bad.push((name, nice(&err)));
            break; // a compressed stream can't go on after damage
        }
        last = name;
    }
    // read the rest too: the compressor's own checksum sits at the very end of the stream
    if bad.is_empty() {
        let mut rest = a.into_inner();
        if let Err(e) = std::io::copy(&mut rest, &mut Sink) {
            bad.push((String::new(), nice(&e.to_string())));
        }
    }
    Ok(bad)
}

fn single(archive: &Path, c: Comp, name: &str, p: &Progress) -> Result<Vec<(String, String)>, String> {
    let mut r = crates::decoder(archive, c)?;
    match copy_counted(&mut r, &mut Sink, p) {
        Ok(()) => Ok(vec![]),
        Err(e) if e == CANCELLED => Err(e),
        Err(e) => Ok(vec![(name.to_string(), nice(&e))]),
    }
}

/// Library wording into something a person can read.
fn nice(e: &str) -> String {
    let l = e.to_lowercase();
    if l.contains("crc") || l.contains("checksum") || l.contains("check") && l.contains("fail") {
        "Damaged (checksum doesn't match)".into()
    } else if l.contains("unexpected end") || l.contains("eof") || l.contains("failed to fill whole buffer") {
        "Cut off (the archive ends too early)".into()
    } else if l.contains("corrupt") || l.contains("invalid") || l.contains("data error") {
        format!("Damaged ({e})")
    } else {
        e.to_string()
    }
}
