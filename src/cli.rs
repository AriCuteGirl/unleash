//! The command line, for people and for other apps (Burrow's Extract here / Compress…):
//!
//!   unleash [ARCHIVE…]                          open archives in windows
//!   unleash x [--to DIR] ARCHIVE…               extract each one (next to it, or into DIR)
//!   unleash a [--format F] [--name N] [--to DIR] [--level fast|normal|best] FILE…
//!                                               compress into one archive (zip by default)
//!
//! x and a show a small progress window and return at once; a running Unleash does the work.

use crate::backend::create::{Format, Level};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Open(Vec<PathBuf>),
    Extract { to: Option<PathBuf>, archives: Vec<PathBuf> },
    Add { format: Format, name: String, to: PathBuf, level: Level, files: Vec<PathBuf> },
    /// `a --dialog`: let the user pick name, format and place in the New archive window
    Pack(Vec<PathBuf>),
    Help,
    Version,
}

pub const HELP: &str = "Usage:
  unleash [ARCHIVE…]                 open archives
  unleash x [--to DIR] ARCHIVE…      extract each archive next to it (or into DIR):
                                     one folder inside stays that folder, loose files get a
                                     folder named after the archive; nothing is overwritten
  unleash a [OPTIONS] FILE…          compress files and folders into one archive
      --format zip|7z|tar.gz|tar.xz|tar.zst   (default zip)
      --name NAME                    archive name without extension
      --to DIR                       where to put it (default: next to the first file)
      --level fast|normal|best       (default normal)
      --dialog                       ask for all of that in the New archive window
";

pub fn parse(args: &[String], cwd: &Path) -> Result<Cmd, String> {
    let abs = |s: &str| {
        let p = Path::new(s);
        if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) }
    };
    let mut it = args.iter().skip(1).peekable(); // skip the program name
    let sub = match it.peek().map(|s| s.as_str()) {
        Some("-h" | "--help" | "help") => return Ok(Cmd::Help),
        Some("--version" | "-V") => return Ok(Cmd::Version),
        Some("x" | "extract") => "x",
        Some("a" | "add" | "compress") => "a",
        _ => "",
    };
    if sub.is_empty() {
        return Ok(Cmd::Open(it.map(|s| s.strip_prefix("file://").map(uri_path).unwrap_or_else(|| abs(s))).collect()));
    }
    it.next();
    let (mut to, mut format, mut name, mut level, mut files) = (None, Format::Zip, None, Level::Normal, vec![]);
    let mut only_files = false;
    let mut dialog = false;
    while let Some(a) = it.next() {
        if only_files || !a.starts_with("--") {
            files.push(abs(a));
            continue;
        }
        let (key, inline) = match a.split_once('=') {
            Some((k, v)) => (k, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        if key == "--" {
            only_files = true;
            continue;
        }
        if sub == "a" && key == "--dialog" {
            dialog = true;
            continue;
        }
        let mut val = || inline.clone().or_else(|| it.next().cloned()).ok_or(format!("{key} needs a value"));
        match (sub, key) {
            (_, "--to") => to = Some(abs(&val()?)),
            ("a", "--format") => {
                let v = val()?;
                format = Format::parse(&v).ok_or(format!("Unknown format {v}: use zip, 7z, tar.gz, tar.xz or tar.zst"))?;
            }
            ("a", "--name") => name = Some(val()?),
            ("a", "--level") => {
                level = match val()?.as_str() {
                    "fast" => Level::Fast,
                    "normal" => Level::Normal,
                    "best" => Level::Best,
                    v => return Err(format!("Unknown level {v}: use fast, normal or best")),
                }
            }
            _ => return Err(format!("Unknown option {key}")),
        }
    }
    if files.is_empty() {
        return Err(if sub == "x" { "Which archive? unleash x ARCHIVE…".into() } else { "What to compress? unleash a FILE…".into() });
    }
    if sub == "x" {
        return Ok(Cmd::Extract { to, archives: files });
    }
    if dialog {
        return Ok(Cmd::Pack(files));
    }
    let name = name.filter(|n| !n.trim().is_empty() && !n.contains('/')).unwrap_or_else(|| default_name(&files));
    let to = to.unwrap_or_else(|| files[0].parent().map(Path::to_path_buf).unwrap_or_else(|| cwd.to_path_buf()));
    Ok(Cmd::Add { format, name, to, level, files })
}

/// One item: its name ("notes.txt" -> "notes", a folder keeps its name); several: the folder
/// they're in.
fn default_name(files: &[PathBuf]) -> String {
    let n = |p: &Path| p.file_name().map(|n| n.to_string_lossy().to_string());
    let name = if files.len() == 1 {
        let f = &files[0];
        if f.is_dir() { n(f) } else { f.file_stem().map(|s| s.to_string_lossy().to_string()) }
    } else {
        files[0].parent().and_then(n)
    };
    name.filter(|s| !s.is_empty()).unwrap_or_else(|| "archive".into())
}

/// file:///a%20b -> /a b (for apps that pass URIs)
fn uri_path(rest: &str) -> PathBuf {
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = rest.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn a(v: &[&str]) -> Vec<String> {
        std::iter::once("unleash").chain(v.iter().copied()).map(String::from).collect()
    }
    #[test]
    fn parses_commands() {
        let cwd = Path::new("/home/u");
        assert_eq!(parse(&a(&[]), cwd).unwrap(), Cmd::Open(vec![]));
        assert_eq!(parse(&a(&["x.zip", "file:///tmp/a%20b.7z"]), cwd).unwrap(), Cmd::Open(vec!["/home/u/x.zip".into(), "/tmp/a b.7z".into()]));
        assert_eq!(parse(&a(&["--help"]), cwd).unwrap(), Cmd::Help);
        assert_eq!(
            parse(&a(&["x", "--to", "out", "a.zip", "/b.7z"]), cwd).unwrap(),
            Cmd::Extract { to: Some("/home/u/out".into()), archives: vec!["/home/u/a.zip".into(), "/b.7z".into()] }
        );
        assert_eq!(parse(&a(&["x", "--", "--weird.zip"]), cwd).unwrap(), Cmd::Extract { to: None, archives: vec!["/home/u/--weird.zip".into()] });
        assert_eq!(
            parse(&a(&["a", "--format=tar.zst", "--name", "pics", "--level", "best", "p/1.jpg", "p/2.jpg"]), cwd).unwrap(),
            Cmd::Add { format: Format::TarZst, name: "pics".into(), to: "/home/u/p".into(), level: Level::Best, files: vec!["/home/u/p/1.jpg".into(), "/home/u/p/2.jpg".into()] }
        );
        // default names
        match parse(&a(&["a", "/d/notes.txt"]), cwd).unwrap() {
            Cmd::Add { name, to, format, .. } => assert_eq!((name.as_str(), to, format), ("notes", PathBuf::from("/d"), Format::Zip)),
            c => panic!("{c:?}"),
        }
        match parse(&a(&["a", "/d/x/1", "/d/x/2"]), cwd).unwrap() {
            Cmd::Add { name, .. } => assert_eq!(name, "x"),
            c => panic!("{c:?}"),
        }
        assert_eq!(parse(&a(&["a", "--dialog", "f", "g"]), cwd).unwrap(), Cmd::Pack(vec!["/home/u/f".into(), "/home/u/g".into()]));
        assert!(parse(&a(&["x"]), cwd).is_err());
        assert!(parse(&a(&["a", "--format", "rar", "f"]), cwd).is_err());
        assert!(parse(&a(&["x", "--bogus", "f"]), cwd).is_err());
    }
}
