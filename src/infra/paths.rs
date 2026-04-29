//! Infrastructure path helpers — deskd filesystem layout.
//!
//! All paths are relative to `$HOME/.deskd/`.

use std::path::{Path, PathBuf};

/// Ensure a directory exists and is owned by `unix_user` (if provided).
///
/// When `deskd serve` runs as root but agents run as a different user
/// (via `unix_user` in workspace.yaml), directories created by the serve
/// process would be owned by root. This helper chowns the directory tree
/// to the target user so agents can write to it.
pub fn ensure_dir_owned(dir: &Path, unix_user: Option<&str>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;

    #[cfg(unix)]
    if let Some(user) = unix_user {
        chown_recursive(dir, user);
    }

    let _ = unix_user; // suppress unused warning on non-unix
    Ok(())
}

/// Recursively chown a directory tree to the given unix user.
#[cfg(unix)]
fn chown_recursive(dir: &Path, user: &str) {
    use std::process::Command;
    // Use chown -R to set ownership on the directory and all contents.
    match Command::new("chown")
        .args(["-R", &format!("{user}:{user}"), &dir.to_string_lossy()])
        .status()
    {
        Ok(s) if !s.success() => {
            tracing::warn!(
                dir = %dir.display(),
                user = %user,
                exit_code = s.code().unwrap_or(-1),
                "chown .deskd directory failed (not running as root?)"
            );
        }
        Err(e) => {
            tracing::warn!(
                dir = %dir.display(),
                user = %user,
                error = %e,
                "failed to run chown on .deskd directory"
            );
        }
        _ => {}
    }
}

/// Where agent state files are stored: `~/.deskd/agents/`.
pub fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    state_dir_in(Path::new(&home))
}

/// Like `state_dir`, but reads the base from an explicit home path instead
/// of `$HOME`. Lets tests place state files under a tempdir without mutating
/// process env (which is unsafe under the parallel test harness; see #423
/// CI failure on PR #428).
pub fn state_dir_in(home: &Path) -> PathBuf {
    let dir = home.join(".deskd").join("agents");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Where agent logs are stored: `~/.deskd/logs/`.
pub fn log_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home).join(".deskd").join("logs");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Where one-shot reminder JSON files are stored: `~/.deskd/reminders/`.
pub fn reminders_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home).join(".deskd").join("reminders");
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// Derive the bus socket path for an agent from its work directory.
/// Convention: `{work_dir}/.deskd/bus.sock`
pub fn agent_bus_socket(work_dir: &str) -> String {
    PathBuf::from(work_dir)
        .join(".deskd")
        .join("bus.sock")
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_dir_owned_creates_directory() {
        let base = std::env::temp_dir().join("deskd-test-ensure-dir-owned");
        let _ = std::fs::remove_dir_all(&base);
        let target = base.join("sub").join("nested");
        assert!(!target.exists());

        ensure_dir_owned(&target, None).unwrap();
        assert!(target.is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ensure_dir_owned_idempotent() {
        let base = std::env::temp_dir().join("deskd-test-ensure-dir-idempotent");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();

        ensure_dir_owned(&base, None).unwrap();
        assert!(base.is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }
}
