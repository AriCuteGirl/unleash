//! Everything else through the `7z` program (7zip package): 7z, rar, iso, cab, wim, dmg, ...

use super::{CANCELLED, Entry, NEEDS_PASSWORD, Progress, WRONG_PASSWORD};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::Duration;

fn base(cmd: &str) -> Command {
    let mut c = Command::new("7z");
    // the password goes in on stdin when 7z asks (never on the command line, where other
    // programs could read it); with none, stdin is closed and 7z stops at once.
    // -sccUTF-8: UTF-8 names in the output
    c.arg(cmd).arg("-sccUTF-8").env("LC_ALL", "C.UTF-8").stdin(Stdio::piped());
    c
}

/// Answer 7z's password question(s) and close stdin.
fn feed(child: &mut std::process::Child, pw: Option<&str>, times: usize) {
    if let Some(mut si) = child.stdin.take() {
        if let Some(pw) = pw {
            use std::io::Write;
            for _ in 0..times {
                let _ = writeln!(si, "{pw}");
            }
        }
    }
}

/// Last useful line of 7z's error output.
fn err_text(s: &str, had_pw: bool) -> String {
    let s = s.trim();
    if s.contains("Wrong password") || (s.contains("encrypted") && had_pw) {
        return WRONG_PASSWORD.into();
    }
    if s.contains("Break signaled") || s.contains("encrypted") || s.contains("Enter password") {
        return if had_pw { WRONG_PASSWORD.into() } else { NEEDS_PASSWORD.into() };
    }
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .find(|l| l.starts_with("ERROR") || l.contains("Can not open") || l.contains("Error"))
        .or_else(|| s.lines().rev().map(str::trim).find(|l| !l.is_empty()))
        .unwrap_or("7z failed")
        .trim_start_matches("ERROR: ")
        .to_string()
}

pub fn list(path: &Path, pw: Option<&str>) -> Result<(String, Vec<Entry>), String> {
    let mut child = base("l").arg("-slt").arg("--").arg(path).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("Couldn't start 7z: {e}"))?;
    feed(&mut child, pw, 1);
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() || text.contains("Break signaled") {
        let mut all = String::from_utf8_lossy(&out.stderr).to_string();
        all.push('\n');
        all.push_str(&text);
        return Err(err_text(&all, pw.is_some()));
    }
    Ok(parse_slt(&text))
}

/// Parse `7z l -slt`: a header block about the archive, then "----------", then one
/// "Key = value" block per entry.
pub fn parse_slt(text: &str) -> (String, Vec<Entry>) {
    let (head, body) = match text.find("\n----------\n") {
        Some(i) => (&text[..i], &text[i + 12..]),
        None => ("", text),
    };
    // the innermost "Type = " before the separator (a tar.gz shows gz then tar)
    let type_name = head.lines().filter_map(|l| l.strip_prefix("Type = ")).last().unwrap_or("7-Zip").to_string();
    let mut out = vec![];
    for block in body.split("\n\n") {
        let mut e = Entry::default();
        let mut has_path = false;
        for l in block.lines() {
            let Some((k, v)) = l.split_once(" = ").or_else(|| l.strip_suffix(" =").map(|k| (k, ""))) else { continue };
            match k {
                "Path" => {
                    e.path = v.to_string();
                    has_path = true;
                }
                "Size" => e.size = v.parse().unwrap_or(0),
                "Packed Size" => e.packed = v.parse().ok(),
                "Modified" => e.modified = v.get(..16).unwrap_or(v).to_string(),
                "Folder" => e.is_dir |= v == "+",
                "Attributes" => e.is_dir |= v.starts_with('D') || v.contains(" d"),
                "Encrypted" => e.encrypted = v == "+",
                "Method" => e.method = v.to_string(),
                _ => {}
            }
        }
        if has_path {
            out.push(e);
        }
    }
    (type_name, out)
}

/// `7z x` into `into`, with progress from its percentage output.
pub fn extract(path: &Path, sel: &[String], into: &Path, p: &Progress, pw: Option<&str>) -> Result<(), String> {
    let mut cmd = base("x");
    cmd.args(["-y", "-bso0", "-bse2", "-bsp1", "-bb0"]).arg(format!("-o{}", into.display())).arg("--").arg(path);
    // whole folders are picked by name; 7z includes what's inside them
    for s in sel {
        cmd.arg(s);
    }
    if !sel.is_empty() {
        cmd.arg("-r-"); // names are exact paths, not searched for in every folder
    }
    run(cmd, p, pw, 1)
}

/// `7z a` of `names` (relative to `dir`) into `out`, with progress.
/// With a password the file names are encrypted too (-mhe), so nothing shows without it.
pub fn create(dir: &Path, names: &[String], out: &Path, level: u32, p: &Progress, pw: Option<&str>) -> Result<(), String> {
    let mut cmd = base("a");
    if pw.is_some() {
        cmd.args(["-p", "-mhe=on"]); // -p alone: 7z asks (twice) on stdin
    }
    cmd.current_dir(dir).args(["-t7z", "-y", "-bso0", "-bse2", "-bsp1", "-bb0", "-snl"]).arg(format!("-mx={level}")).arg("--").arg(out).args(names);
    run(cmd, p, pw, 2)
}

/// `7z t`: Ok(list of damaged files with the reason), or an error about the archive itself.
pub fn test(path: &Path, p: &Progress, pw: Option<&str>) -> Result<Vec<(String, String)>, String> {
    let mut cmd = base("t");
    // errors on stdout (-bse1) to collect, progress on stderr (-bsp2) to follow
    cmd.args(["-bso0", "-bse1", "-bsp2", "-bb0"]).arg("--").arg(path);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start 7z: {e}"))?;
    feed(&mut child, pw, 1);
    let mut so = child.stdout.take().ok_or("no output")?;
    let mut se = child.stderr.take().ok_or("no output")?;
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let total = p.total.load(Ordering::Relaxed);
    let st = std::thread::scope(|sc| {
        sc.spawn(|| {
            let mut buf = [0u8; 4096];
            let mut pending = String::new();
            while let Ok(n) = se.read(&mut buf) {
                if n == 0 {
                    break;
                }
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                let parts: Vec<&str> = pending.split(['\x08', '\r', '\n']).collect();
                for part in &parts[..parts.len() - 1] {
                    if let Some(pc) = percent(part) {
                        p.done.store(total * pc / 100, Ordering::Relaxed);
                        if let Some((_, name)) = part.split_once(" - ") {
                            p.set_current(name.trim());
                        }
                    }
                }
                pending = parts[parts.len() - 1].to_string();
            }
        });
        loop {
            if p.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CANCELLED.to_string());
            }
            match child.try_wait() {
                Ok(Some(st)) => return Ok(st),
                Ok(None) => std::thread::sleep(Duration::from_millis(60)),
                Err(e) => return Err(e.to_string()),
            }
        }
    })?;
    let text = out.join().unwrap_or_default();
    let bad = parse_test_errors(&text);
    if st.success() || !bad.is_empty() {
        if text.contains("Wrong password") {
            return Err(WRONG_PASSWORD.into());
        }
        return Ok(bad);
    }
    Err(err_text(&text, pw.is_some()))
}

/// "ERROR: CRC Failed : dir/a.bin" -> ("dir/a.bin", "CRC Failed")
pub fn parse_test_errors(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| l.trim().strip_prefix("ERROR: "))
        .filter_map(|l| l.rsplit_once(" : ").map(|(why, path)| (path.trim().to_string(), why.trim().to_string())))
        .filter(|(path, _)| !path.is_empty())
        .collect()
}

/// Rename entries: (old path, new path) pairs. A folder takes what's inside along.
pub fn rename(archive: &Path, pairs: &[(String, String)]) -> Result<(), String> {
    let mut cmd = Command::new("7z");
    cmd.args(["rn", "-sccUTF-8", "-y", "-bso0", "-bsp0"]).env("LC_ALL", "C.UTF-8").stdin(Stdio::null()).arg("--").arg(archive);
    for (a, b) in pairs {
        cmd.arg(a).arg(b);
    }
    let out = cmd.output().map_err(|e| format!("Couldn't start 7z: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(err_text(&String::from_utf8_lossy(&out.stderr), false)) }
}

/// Add `names` (relative to `dir`) to an existing archive, keeping its format. `follow`: store
/// what symlinks point to (used for staged links), else store links as links.
pub fn update(dir: &Path, names: &[String], archive: &Path, follow: bool, p: &Progress) -> Result<(), String> {
    let mut cmd = Command::new("7z");
    cmd.arg("a").arg("-sccUTF-8").env("LC_ALL", "C.UTF-8").stdin(Stdio::piped()).current_dir(dir);
    cmd.args(["-y", "-bso0", "-bse2", "-bsp1", "-bb0"]);
    if !follow {
        cmd.arg("-snl");
    }
    let archive = std::path::absolute(archive).map_err(|e| e.to_string())?;
    cmd.arg("--").arg(archive).args(names);
    run(cmd, p, None, 0)
}

/// Delete entries (folders with their contents) from an archive.
pub fn delete(archive: &Path, paths: &[String]) -> Result<(), String> {
    let out = Command::new("7z")
        .args(["d", "-sccUTF-8", "-y", "-bso0", "-bsp0", "-r-"])
        .env("LC_ALL", "C.UTF-8")
        .stdin(Stdio::null())
        .arg("--")
        .arg(archive)
        .args(paths)
        .output()
        .map_err(|e| format!("Couldn't start 7z: {e}"))?;
    if out.status.success() { Ok(()) } else { Err(err_text(&String::from_utf8_lossy(&out.stderr), false)) }
}

/// Run 7z, read its percentage into `p`, kill it on cancel.
fn run(mut cmd: Command, p: &Progress, pw: Option<&str>, asks: usize) -> Result<(), String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start 7z: {e}"))?;
    feed(&mut child, pw, asks);
    let mut so = child.stdout.take().ok_or("no output")?;
    let mut se = child.stderr.take();
    let errs = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = se.as_mut() {
            let _ = e.read_to_string(&mut s);
        }
        s
    });
    let total = p.total.load(Ordering::Relaxed);
    let prog = std::thread::scope(|sc| {
        sc.spawn(|| {
            // "  42% 3 - name" rewritten in place with backspaces
            let mut buf = [0u8; 4096];
            let mut pending = String::new();
            while let Ok(n) = so.read(&mut buf) {
                if n == 0 {
                    break;
                }
                pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                let parts: Vec<&str> = pending.split(['\x08', '\r', '\n']).collect();
                for part in &parts[..parts.len() - 1] {
                    if let Some(pc) = percent(part) {
                        p.done.store(total * pc / 100, Ordering::Relaxed);
                        if let Some((_, name)) = part.split_once(" - ") {
                            p.set_current(name.trim());
                        }
                    }
                }
                pending = parts[parts.len() - 1].to_string();
            }
        });
        loop {
            if p.cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CANCELLED.to_string());
            }
            match child.try_wait() {
                Ok(Some(st)) => return Ok(st),
                Ok(None) => std::thread::sleep(Duration::from_millis(60)),
                Err(e) => return Err(e.to_string()),
            }
        }
    });
    let st = prog?;
    let msg = errs.join().unwrap_or_default();
    if st.success() {
        p.done.store(total, Ordering::Relaxed);
        Ok(())
    } else {
        Err(err_text(&msg, pw.is_some()))
    }
}

fn percent(s: &str) -> Option<u64> {
    let s = s.trim_start();
    let (n, _) = s.split_once('%')?;
    n.trim().parse().ok().filter(|v: &u64| *v <= 100)
}
