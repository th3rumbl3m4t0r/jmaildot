//! Passwords and API keys: the desktop keyring (Secret Service, through `secret-tool`) when
//! there is one, else a file only the user can read. `where_stored` says which.

use crate::paths;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::io::Write;
use std::process::{Command, Stdio};

const SERVICE: &str = "maildot";

fn file() -> std::path::PathBuf {
    paths::data_dir().join("secrets.json")
}

fn read_file() -> BTreeMap<String, String> {
    std::fs::read_to_string(file()).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_file(m: &BTreeMap<String, String>) -> Result<()> {
    let p = file();
    if let Some(d) = p.parent() {
        paths::ensure(d)?;
    }
    let tmp = p.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(serde_json::to_string_pretty(m)?.as_bytes())?;
    }
    std::fs::rename(tmp, p)?;
    Ok(())
}

fn keyring_disabled() -> bool {
    std::env::var_os("MAILDOT_NO_KEYRING").is_some()
}

fn tool_store(key: &str, value: &str) -> Result<()> {
    if keyring_disabled() {
        bail!("keyring off");
    }
    let mut c = Command::new("secret-tool")
        .args(["store", "--label", &format!("maildot: {key}"), "service", SERVICE, "key", key])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    c.stdin.as_mut().unwrap().write_all(value.as_bytes())?;
    drop(c.stdin.take());
    if !c.wait()?.success() {
        bail!("secret-tool store failed");
    }
    Ok(())
}

fn tool_lookup(key: &str) -> Option<String> {
    if keyring_disabled() {
        return None;
    }
    let o = Command::new("secret-tool").args(["lookup", "service", SERVICE, "key", key]).stderr(Stdio::null()).output().ok()?;
    if o.status.success() && !o.stdout.is_empty() {
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    } else {
        None
    }
}

fn tool_clear(key: &str) {
    if keyring_disabled() {
        return;
    }
    let _ = Command::new("secret-tool").args(["clear", "service", SERVICE, "key", key]).stderr(Stdio::null()).status();
}

/// Stores a secret; true when it went to the keyring, false when to the file.
pub fn set(key: &str, value: &str) -> Result<bool> {
    if tool_store(key, value).is_ok() && tool_lookup(key).as_deref() == Some(value) {
        let mut m = read_file();
        if m.remove(key).is_some() {
            write_file(&m)?;
        }
        return Ok(true);
    }
    let mut m = read_file();
    m.insert(key.to_string(), value.to_string());
    write_file(&m)?;
    Ok(false)
}

pub fn get(key: &str) -> Option<String> {
    tool_lookup(key).or_else(|| read_file().get(key).cloned())
}

pub fn remove(key: &str) {
    tool_clear(key);
    let mut m = read_file();
    if m.remove(key).is_some() {
        let _ = write_file(&m);
    }
}

/// "keyring" or "file".
pub fn where_stored(key: &str) -> &'static str {
    if tool_lookup(key).is_some() {
        "keyring"
    } else if read_file().contains_key(key) {
        "file (0600)"
    } else {
        "nowhere"
    }
}
