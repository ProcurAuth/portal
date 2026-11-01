//! Secret files. Contents never appear in errors or logs.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use zeroize::Zeroizing;

pub fn read_secret(path: &Path, label: &str) -> Result<Zeroizing<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{label}: cannot read {}: {e}", path.display()))?;
    let text = Zeroizing::new(text);
    let trimmed = Zeroizing::new(text.trim().to_string());
    if trimmed.is_empty() {
        return Err(format!("{label}: {} is empty", path.display()));
    }
    Ok(trimmed)
}

/// Writes a new secret file with mode 0600. Never overwrites.
pub fn write_new_secret(path: &Path, contents: &str) -> Result<(), String> {
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    f.write_all(contents.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .and_then(|_| f.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}
