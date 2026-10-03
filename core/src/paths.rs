//! Where things live on disk. `MAILDOT_HOME` moves everything (tests, a second profile).

use std::path::PathBuf;

pub fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn data_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("MAILDOT_HOME") {
        return PathBuf::from(d);
    }
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).unwrap_or_else(|| home().join(".local/share"));
    base.join("maildot")
}

pub fn db_path() -> PathBuf {
    data_dir().join("maildot.db")
}

pub fn mail_dir(account: i64) -> PathBuf {
    data_dir().join("mail").join(account.to_string())
}

/// The decrypted message, as a plain RFC 5322 file (grep works on these too).
pub fn eml_path(account: i64, id: i64) -> PathBuf {
    mail_dir(account).join(format!("{:03}", id % 1000)).join(format!("{id}.eml"))
}

/// Attachments opened with another program go here; emptied at start.
pub fn tmp_dir() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| data_dir().join("run"));
    base.join("maildot")
}

pub fn ensure(p: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(p)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}
