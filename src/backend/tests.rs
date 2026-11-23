use super::*;
use std::fs;
use std::process::Command;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("unleash-be-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

/// src/proj/{readme.md, src/main.rs, src/deep/x.txt}, src/loose.txt
fn tree(d: &Path) -> PathBuf {
    let s = d.join("src");
    fs::create_dir_all(s.join("proj/src/deep")).unwrap();
    fs::write(s.join("proj/readme.md"), "# hi").unwrap();
    fs::write(s.join("proj/src/main.rs"), "fn main(){}").unwrap();
    fs::write(s.join("proj/src/deep/x.txt"), "x".repeat(70_000)).unwrap();
    fs::write(s.join("loose.txt"), "loose").unwrap();
    s
}

fn run(c: &mut Command) {
    let o = c.output().unwrap();
    assert!(o.status.success(), "{c:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Build an archive of `src` with the system tools (as other apps would make them).
fn make(d: &Path, src: &Path, name: &str) -> Option<PathBuf> {
    let out = d.join(name);
    if name.ends_with(".7z") {
        if !have("7z") {
            return None;
        }
        run(Command::new("7z").current_dir(src).args(["a", "-bd", "-y"]).arg(&out).args(["proj", "loose.txt"]));
    } else {
        if !have("bsdtar") {
            return None;
        }
        run(Command::new("bsdtar").current_dir(src).arg("-acf").arg(&out).args(["proj", "loose.txt"]));
    }
    Some(out)
}

fn paths(l: &Listing) -> Vec<String> {
    let mut v: Vec<String> = l.entries.iter().filter(|e| !e.is_dir).map(|e| e.path.clone()).collect();
    v.sort();
    v
}

#[test]
fn path_rules() {
    assert_eq!(clean_path("./a//b/").as_deref(), Some("a/b"));
    assert_eq!(clean_path("/abs/x").as_deref(), Some("abs/x"));
    assert_eq!(clean_path("a/../../x"), None);
    assert_eq!(clean_path("./"), None);
    let sel = vec!["a/b".to_string()];
    assert!(selected("a/b", &sel) && selected("a/b/c", &sel));
    assert!(!selected("a/bc", &sel) && !selected("a", &sel));
    assert!(selected("anything", &[]));
}

#[test]
fn stems() {
    assert_eq!(stem("photos.tar.gz"), "photos");
    assert_eq!(stem("notes.txt.gz"), "notes.txt");
    assert_eq!(stem("Game.v1.2.ZIP"), "Game.v1.2");
    assert_eq!(stem("big.7z.001"), "big");
    assert_eq!(stem("movie.part01.rar"), "movie");
    assert_eq!(stem("plain"), "plain");
}

#[test]
fn every_format_lists_and_extracts() {
    let d = tmp("formats");
    let src = tree(&d);
    let want = vec!["loose.txt", "proj/readme.md", "proj/src/deep/x.txt", "proj/src/main.rs"];
    for (name, kind) in [
        ("a.zip", Kind::Zip),
        ("a.tar", Kind::Tar(Comp::None)),
        ("a.tar.gz", Kind::Tar(Comp::Gz)),
        ("a.tar.zst", Kind::Tar(Comp::Zst)),
        ("a.tar.xz", Kind::Tar(Comp::Xz)),
        ("a.tar.bz2", Kind::BsdTar),
        ("a.7z", Kind::SevenZ),
    ] {
        let Some(arc) = make(&d, &src, name) else { continue };
        let l = list(&arc).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(l.kind, kind, "{name}");
        assert_eq!(paths(&l), want, "{name}");
        let x = l.entries.iter().find(|e| e.path == "proj/src/deep/x.txt").unwrap();
        assert_eq!(x.size, 70_000, "{name}");
        assert!(x.modified.len() == 16, "{name}: {:?}", x.modified);

        // everything, smart: two things at the top -> a folder named after the archive
        let out = d.join(format!("out-{name}"));
        fs::create_dir_all(&out).unwrap();
        let p = Progress::default();
        let made = extract_to(&arc, &l, &[], "", &out, Placement::Smart, &p).unwrap();
        assert_eq!(made, vec![out.join("a")], "{name}");
        assert_eq!(fs::read_to_string(out.join("a/proj/src/main.rs")).unwrap(), "fn main(){}");
        assert_eq!(fs::read_to_string(out.join("a/proj/src/deep/x.txt")).unwrap().len(), 70_000);
        assert!(p.fraction() > 0.99, "{name}: {}", p.fraction());

        // a selection inside a subfolder lands without the folders above it
        let made = extract_to(&arc, &l, &["proj/src/deep".into(), "proj/src/main.rs".into()], "proj/src", &out, Placement::Loose, &p).unwrap();
        let mut names: Vec<_> = made.iter().map(|m| m.file_name().unwrap().to_string_lossy().to_string()).collect();
        names.sort();
        assert_eq!(names, ["deep", "main.rs"], "{name}");
        assert!(out.join("deep/x.txt").is_file() && !out.join("readme.md").exists());
        // again: free names, nothing overwritten
        let made = extract_to(&arc, &l, &["proj/src/main.rs".into()], "proj/src", &out, Placement::Loose, &p).unwrap();
        assert_eq!(made, vec![out.join("main (2).rs")], "{name}");
        // no temp folders left behind
        assert!(fs::read_dir(&out).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("unleash-part")));
    }
}

#[test]
fn single_compressed_file() {
    let d = tmp("single");
    fs::write(d.join("notes.txt"), "hello ".repeat(1000)).unwrap();
    let f = File::create(d.join("notes.txt.gz")).unwrap();
    let mut gz = flate2::write::GzEncoder::new(f, flate2::Compression::default());
    std::io::Write::write_all(&mut gz, "hello ".repeat(1000).as_bytes()).unwrap();
    gz.finish().unwrap();
    let l = list(&d.join("notes.txt.gz")).unwrap();
    assert_eq!(l.kind, Kind::Single(Comp::Gz));
    assert_eq!(l.entries[0].path, "notes.txt");
    assert_eq!(l.entries[0].size, 6000);
    let out = d.join("out");
    fs::create_dir(&out).unwrap();
    let made = extract_to(&d.join("notes.txt.gz"), &l, &[], "", &out, Placement::Smart, &Progress::default()).unwrap();
    assert_eq!(made, vec![out.join("notes.txt")]);
    assert_eq!(fs::read_to_string(&made[0]).unwrap().len(), 6000);
}
use std::fs::File;

#[test]
fn hostile_paths_stay_inside() {
    let d = tmp("slip");
    // a tar with "../evil.txt" and "/abs.txt" written by hand (the tar crate refuses to)
    let mut b = tar::Builder::new(Vec::new());
    for name in ["../evil.txt", "ok/../../evil2.txt", "fine.txt"] {
        let mut h = tar::Header::new_ustar();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_size(4);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append(&h, &b"evil"[..]).unwrap();
    }
    fs::write(d.join("bad.tar"), b.into_inner().unwrap()).unwrap();
    let l = list(&d.join("bad.tar")).unwrap();
    assert_eq!(paths(&l), ["fine.txt"]);
    let out = d.join("out");
    fs::create_dir(&out).unwrap();
    extract_to(&d.join("bad.tar"), &l, &[], "", &out, Placement::Smart, &Progress::default()).unwrap();
    assert!(out.join("fine.txt").is_file());
    assert!(!d.join("evil.txt").exists() && !d.join("evil2.txt").exists());
}

#[test]
fn cancel_and_errors_leave_nothing() {
    let d = tmp("cancel");
    let src = tree(&d);
    let Some(arc) = make(&d, &src, "c.tar.gz") else { return };
    let l = list(&arc).unwrap();
    let out = d.join("out");
    fs::create_dir(&out).unwrap();
    let p = Progress::default();
    p.cancel.store(true, Ordering::Relaxed);
    assert_eq!(extract_to(&arc, &l, &[], "", &out, Placement::Smart, &p).unwrap_err(), CANCELLED);
    assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
    fs::write(d.join("junk.zip"), "PK\x03\x04 definitely not a zip").unwrap();
    assert!(list(&d.join("junk.zip")).is_err());
}

#[test]
fn parses_7z_technical_listing() {
    let text = "--\nPath = x.tar.gz\nType = gzip\n\nPath = x.tar\nType = tar\n\n----------\nPath = dir\nFolder = +\nSize = 0\nModified = 2025-01-02 03:04:05.123\n\nPath = dir/a b.txt\nFolder = -\nSize = 12\nPacked Size = \nModified = 2025-01-02 03:04:05\nEncrypted = +\nMethod = AES\n\n";
    let (t, e) = sevenz::parse_slt(text);
    assert_eq!(t, "tar");
    assert_eq!(e.len(), 2);
    assert!(e[0].is_dir);
    assert_eq!(e[1].path, "dir/a b.txt");
    assert_eq!((e[1].size, e[1].packed, e[1].encrypted), (12, None, true));
    assert_eq!(e[1].modified, "2025-01-02 03:04");
}

#[test]
fn create_round_trips_every_format() {
    use create::{Format, Level};
    let d = tmp("create");
    let src = tree(&d);
    std::os::unix::fs::symlink("readme.md", src.join("proj/link.md")).unwrap();
    let items = [src.join("proj"), src.join("loose.txt")];
    for fmt in Format::ALL {
        if fmt == Format::SevenZ && !have("7z") {
            continue;
        }
        let p = Progress::default();
        let arc = create::create(&items, &d, "out", fmt, Level::Normal, &p).unwrap_or_else(|e| panic!("{fmt:?}: {e}"));
        assert_eq!(arc, d.join(format!("out{}", fmt.ext())), "{fmt:?}");
        assert!(p.fraction() > 0.99, "{fmt:?}: {}", p.fraction());
        let l = list(&arc).unwrap();
        assert_eq!(paths(&l), ["loose.txt", "proj/link.md", "proj/readme.md", "proj/src/deep/x.txt", "proj/src/main.rs"], "{fmt:?}");
        let out = d.join(format!("x-{}", fmt.ext().trim_start_matches('.')));
        fs::create_dir(&out).unwrap();
        extract_to(&arc, &l, &[], "", &out, Placement::Loose, &Progress::default()).unwrap();
        assert_eq!(fs::read_to_string(out.join("proj/src/deep/x.txt")).unwrap().len(), 70_000, "{fmt:?}");
        assert_eq!(fs::read_link(out.join("proj/link.md")).unwrap(), Path::new("readme.md"), "{fmt:?}");
        // a second one gets a free name
        let again = create::create(&items[1..], &d, "out", fmt, Level::Fast, &p).unwrap();
        assert_eq!(again, d.join(format!("out (2){}", fmt.ext())));
    }
    assert!(fs::read_dir(&d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains("unleash-part")));
}

#[test]
fn create_cancel_and_clashes_leave_nothing() {
    use create::{Format, Level};
    let d = tmp("create-cancel");
    let src = tree(&d);
    let p = Progress::default();
    p.cancel.store(true, Ordering::Relaxed);
    for fmt in [Format::Zip, Format::TarZst] {
        assert_eq!(create::create(&[src.join("proj")], &d, "c", fmt, Level::Normal, &p).unwrap_err(), CANCELLED);
    }
    fs::create_dir_all(d.join("other")).unwrap();
    fs::write(d.join("other/loose.txt"), "x").unwrap();
    assert!(create::create(&[src.join("loose.txt"), d.join("other/loose.txt")], &d, "c", Format::Zip, Level::Normal, &Progress::default()).is_err());
    let names: Vec<_> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert!(names.iter().all(|n| !n.starts_with('c') && !n.contains("unleash-part")), "{names:?}");
    assert_eq!(create::Format::parse(".tar.zst"), Some(Format::TarZst));
    assert_eq!(create::Format::parse("7Z"), Some(Format::SevenZ));
}
