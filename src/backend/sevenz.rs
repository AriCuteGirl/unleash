//! Everything else through the `7z` program (7zip package): 7z, rar, iso, cab, wim, dmg, ...

use super::{CANCELLED, Entry, Progress};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::Duration;

fn base(cmd: &str) -> Command {
    let mut c = Command::new("7z");
    // -p with an empty value: an encrypted archive fails at once instead of waiting on stdin
    // (password support comes with the keyring milestone). -sccUTF-8: UTF-8 names in output.
    c.arg(cmd).arg("-p").arg("-sccUTF-8").env("LC_ALL", "C.UTF-8").stdin(Stdio::null());
    c
}

/// Last useful line of 7z's error output.
fn err_text(s: &str) -> String {
    let s = s.trim();
    if s.contains("Wrong password") || s.contains("encrypted") {
        return "This archive needs a password (coming in a later version)".into();
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

pub fn list(path: &Path) -> Result<(String, Vec<Entry>), String> {
    let out = base("l").arg("-slt").arg("--").arg(path).output().map_err(|e| format!("Couldn't start 7z: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let mut all = String::from_utf8_lossy(&out.stderr).to_string();
        all.push('\n');
        all.push_str(&text);
        return Err(err_text(&all));
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
pub fn extract(path: &Path, sel: &[String], into: &Path, p: &Progress) -> Result<(), String> {
    let mut cmd = base("x");
    cmd.args(["-y", "-bso0", "-bse2", "-bsp1", "-bb0"]).arg(format!("-o{}", into.display())).arg("--").arg(path);
    // whole folders are picked by name; 7z includes what's inside them
    for s in sel {
        cmd.arg(s);
    }
    if !sel.is_empty() {
        cmd.arg("-r-"); // names are exact paths, not searched for in every folder
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("Couldn't start 7z: {e}"))?;
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
        Err(err_text(&msg))
    }
}

fn percent(s: &str) -> Option<u64> {
    let s = s.trim_start();
    let (n, _) = s.split_once('%')?;
    n.trim().parse().ok().filter(|v: &u64| *v <= 100)
}
