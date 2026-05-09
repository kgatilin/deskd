//! HMAC secret loader / generator for the web adapter (#443).
//!
//! Reads `~/.deskd/web-secret` (32 bytes, raw). If the file does not exist,
//! generates a fresh 32-byte random secret using the OS RNG and writes it
//! with mode `0600` so only the agent's unix user can read it.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Length of the HMAC secret in bytes.
pub const SECRET_LEN: usize = 32;

/// Load the existing web secret or generate and persist a new one.
///
/// Path resolution: `<home>/.deskd/web-secret`, where `home` defaults to
/// `$HOME` (falling back to `/tmp` for tests when `HOME` is unset, mirroring
/// the convention used in `infra::paths`).
pub fn load_or_create() -> Result<[u8; SECRET_LEN]> {
    let path = secret_path();
    load_or_create_at(&path)
}

/// Path of the persisted secret. Public so tests can stub via tempdirs.
pub fn secret_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".deskd").join("web-secret")
}

/// Like [`load_or_create`], but reads/writes at an explicit path.
pub fn load_or_create_at(path: &Path) -> Result<[u8; SECRET_LEN]> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create parent dir for web-secret: {}", parent.display()))?;
    }

    if path.exists() {
        let bytes =
            std::fs::read(path).with_context(|| format!("read web-secret: {}", path.display()))?;
        if bytes.len() != SECRET_LEN {
            anyhow::bail!(
                "web-secret at {} has wrong length {} (expected {})",
                path.display(),
                bytes.len(),
                SECRET_LEN
            );
        }
        let mut out = [0u8; SECRET_LEN];
        out.copy_from_slice(&bytes);
        return Ok(out);
    }

    // Generate fresh 32 bytes from OS RNG via ring (already a dep).
    use ring::rand::SecureRandom;
    let rng = ring::rand::SystemRandom::new();
    let mut buf = [0u8; SECRET_LEN];
    rng.fill(&mut buf)
        .map_err(|_| anyhow::anyhow!("OS RNG failure generating web-secret"))?;

    write_secret(path, &buf)?;
    Ok(buf)
}

#[cfg(unix)]
fn write_secret(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create web-secret: {}", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("write web-secret: {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_secret(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("write web-secret: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn generates_secret_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".deskd").join("web-secret");
        assert!(!path.exists());
        let s = load_or_create_at(&path).unwrap();
        assert_eq!(s.len(), SECRET_LEN);
        assert!(path.exists());
    }

    #[test]
    fn returns_existing_secret_on_second_call() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("ws").join("web-secret");
        let s1 = load_or_create_at(&path).unwrap();
        let s2 = load_or_create_at(&path).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn generated_secret_is_non_zero() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-secret");
        let s = load_or_create_at(&path).unwrap();
        assert!(s.iter().any(|&b| b != 0));
    }

    #[test]
    fn rejects_wrong_length_secret() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-secret");
        std::fs::write(&path, b"too short").unwrap();
        let res = load_or_create_at(&path);
        assert!(res.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn generated_secret_has_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("web-secret");
        let _ = load_or_create_at(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        // Lower 9 bits should be exactly rw for owner only.
        assert_eq!(mode & 0o777, 0o600, "expected 0600, got {:o}", mode & 0o777);
    }
}
