//! Remembered archive passwords live only in the keyring (Secret Service: gnome-keyring,
//! KeePassXC, KWallet's compat layer), through libsecret's `secret-tool`. The password goes over
//! stdin/stdout, never on a command line. Call these from a worker thread.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const APP: &str = "world.puppygirl.Unleash";

fn have() -> bool {
    crate::backend::have("secret-tool")
}

/// The remembered password for this archive (by its full path), if any.
pub fn lookup(archive: &Path) -> Option<String> {
    if !have() {
        return None;
    }
    let out = Command::new("secret-tool")
        .args(["lookup", "application", APP, "archive"])
        .arg(archive)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let pw = String::from_utf8(out.stdout).ok()?;
    (out.status.success() && !pw.is_empty()).then_some(pw)
}

pub fn store(archive: &Path, pw: &str) -> Result<(), String> {
    if !have() {
        return Err("secret-tool isn't installed (libsecret)".into());
    }
    let name = archive.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let mut child = Command::new("secret-tool")
        .args(["store", "--label"])
        .arg(format!("Unleash: {name}"))
        .args(["application", APP, "archive"])
        .arg(archive)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(pw.as_bytes()).map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&out.stderr).trim().to_string()) }
}

/// Forget it (after it turned out wrong, e.g. the archive was replaced).
pub fn forget(archive: &Path) {
    if have() {
        let _ = Command::new("secret-tool").args(["clear", "application", APP, "archive"]).arg(archive).stdin(Stdio::null()).status();
    }
}
