//! Agent scope backend trait and built-in implementations.
//!
//! Phase 1 of hierarchical agent scopes (#383). Defines the `ScopeBackend`
//! trait that maps abstract scopes to concrete OS-level isolation, and provides
//! a `UnixUserBackend` implementation (the default).

use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Information about a provisioned scope, returned by `ScopeBackend::describe`.
#[derive(Debug, Clone)]
pub struct ScopeInfo {
    pub scope_type: String,
    pub work_dir: PathBuf,
    pub user: Option<String>,
    pub env_keys: Vec<String>,
}

/// Handle to a provisioned scope, used to spawn processes and check access.
#[derive(Debug, Clone)]
pub struct ScopeHandle {
    pub scope_path: String,
    pub work_dir: PathBuf,
    pub user: Option<String>,
    pub env: HashMap<String, String>,
}

/// Pluggable scope backend trait. Maps abstract scope to OS-level isolation.
///
/// Phase 1 implements only `UnixUserBackend`. Future phases will add
/// container, tmpfs, and overlay backends.
#[allow(async_fn_in_trait)]
pub trait ScopeBackend: Send + Sync {
    /// Prepare the scope environment (create user, start container, mount tmpfs, etc.)
    async fn provision(
        &self,
        scope_path: &str,
        work_dir: &Path,
        env: &HashMap<String, String>,
    ) -> Result<ScopeHandle>;

    /// Check if a path is accessible within this scope.
    fn can_access(&self, handle: &ScopeHandle, path: &Path) -> bool;

    /// Get the effective work_dir for this scope.
    fn work_dir<'a>(&self, handle: &'a ScopeHandle) -> &'a Path;

    /// Introspect scope resources (for get_scope MCP tool).
    fn describe(&self, handle: &ScopeHandle) -> ScopeInfo;

    /// Tear down the scope (stop container, unmount, etc.)
    async fn deprovision(&self, handle: &ScopeHandle) -> Result<()>;
}

/// Default scope backend: maps scope to Unix user + file permissions.
///
/// In Phase 1 this is a lightweight implementation: it validates paths
/// but does not actually switch Unix users (that requires deskd to run as root).
pub struct UnixUserBackend;

impl ScopeBackend for UnixUserBackend {
    async fn provision(
        &self,
        scope_path: &str,
        work_dir: &Path,
        env: &HashMap<String, String>,
    ) -> Result<ScopeHandle> {
        // Ensure work_dir exists.
        if !work_dir.exists() {
            std::fs::create_dir_all(work_dir)?;
        }
        Ok(ScopeHandle {
            scope_path: scope_path.to_string(),
            work_dir: work_dir.to_path_buf(),
            user: None,
            env: env.clone(),
        })
    }

    fn can_access(&self, handle: &ScopeHandle, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let scope_dir = handle
            .work_dir
            .canonicalize()
            .unwrap_or_else(|_| handle.work_dir.clone());
        canonical.starts_with(&scope_dir)
    }

    fn work_dir<'a>(&self, handle: &'a ScopeHandle) -> &'a Path {
        &handle.work_dir
    }

    fn describe(&self, handle: &ScopeHandle) -> ScopeInfo {
        ScopeInfo {
            scope_type: "unix-user".to_string(),
            work_dir: handle.work_dir.clone(),
            user: handle.user.clone(),
            env_keys: handle.env.keys().cloned().collect(),
        }
    }

    async fn deprovision(&self, _handle: &ScopeHandle) -> Result<()> {
        // Unix-user backend: nothing to tear down (process cleanup is handled elsewhere).
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn unix_user_can_access_within_scope() {
        let handle = ScopeHandle {
            scope_path: "/dev".into(),
            work_dir: PathBuf::from("/tmp"),
            user: None,
            env: HashMap::new(),
        };
        let backend = UnixUserBackend;
        // /tmp/child is within /tmp
        assert!(backend.can_access(&handle, Path::new("/tmp/child")));
        assert!(backend.can_access(&handle, Path::new("/tmp")));
    }

    #[test]
    fn unix_user_denies_outside_scope() {
        let handle = ScopeHandle {
            scope_path: "/dev".into(),
            work_dir: PathBuf::from("/tmp/sandbox"),
            user: None,
            env: HashMap::new(),
        };
        let backend = UnixUserBackend;
        // /etc is outside /tmp/sandbox
        assert!(!backend.can_access(&handle, Path::new("/etc/passwd")));
    }

    #[test]
    fn unix_user_describe() {
        let mut env = HashMap::new();
        env.insert("API_KEY".into(), "secret".into());
        let handle = ScopeHandle {
            scope_path: "/dev".into(),
            work_dir: PathBuf::from("/home/dev"),
            user: Some("dev".into()),
            env,
        };
        let backend = UnixUserBackend;
        let info = backend.describe(&handle);
        assert_eq!(info.scope_type, "unix-user");
        assert_eq!(info.user, Some("dev".into()));
        assert!(info.env_keys.contains(&"API_KEY".to_string()));
    }

    #[tokio::test]
    async fn unix_user_provision_and_deprovision() {
        let backend = UnixUserBackend;
        let env = HashMap::new();
        let handle = backend
            .provision("/test", Path::new("/tmp"), &env)
            .await
            .unwrap();
        assert_eq!(handle.scope_path, "/test");
        assert!(backend.deprovision(&handle).await.is_ok());
    }
}
