//! Single-instance enforcement via an OS-level file lock.
//!
//! Two `gh-monitor` processes would both poll GitHub (doubling the
//! rate-limit pressure) and both try to grab the system tray icon.
//! Acquiring a `flock`-style exclusive lock on a sentinel file in the
//! config directory prevents a second instance from starting while
//! the first is alive. The lock is released when the [`SingleInstance`]
//! value is dropped (i.e. on normal process exit, on panic unwind, or
//! on `std::process::exit` after the file handle is closed by the OS).
//!
//! We use the standard library's [`std::fs::File::try_lock`] (stable
//! since Rust 1.89). On Unix it maps to `flock` with `LOCK_EX |
//! LOCK_NB`; on Windows it maps to `LockFileEx` with
//! `LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY`. Both release
//! the lock automatically when the file handle is closed.
//!
//! [`std::fs::File::try_lock`]: https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

use thiserror::Error;

/// A held single-instance lock. Drop to release.
#[derive(Debug)]
pub struct SingleInstance {
    /// Kept alive only to hold the OS-level lock. Closing the file
    /// handle (via Drop) releases the lock.
    _file: File,
    /// The lock file path. Stored so the error message can name it.
    pub path: PathBuf,
}

/// Returned by [`SingleInstance::new`] when the lock is already held
/// by another process.
#[derive(Debug, Error)]
#[error("another instance of gh-monitor is already running; lock: {path}", path = .path.display())]
pub struct AlreadyRunning {
    /// The lock file path that was already taken.
    pub path: PathBuf,
}

impl SingleInstance {
    /// Try to acquire the single-instance lock at `path`. On
    /// success, returns a guard that holds the lock until dropped. On
    /// failure (the lock is held by another process, or the file
    /// can't be opened), returns [`AlreadyRunning`] with the path.
    pub fn new(path: &Path) -> Result<Self, AlreadyRunning> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    tracing::warn!(error = %e, path = %parent.display(), "failed to create lockfile parent");
                    return Err(AlreadyRunning {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
        let file = match OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)
        {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(error = %e, path = %path.display(), "failed to open lockfile");
                return Err(AlreadyRunning {
                    path: path.to_path_buf(),
                });
            }
        };
        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                path: path.to_path_buf(),
            }),
            Err(TryLockError::WouldBlock) => Err(AlreadyRunning {
                path: path.to_path_buf(),
            }),
            Err(TryLockError::Error(e)) => {
                tracing::warn!(error = %e, "try_lock failed; treating as already running");
                Err(AlreadyRunning {
                    path: path.to_path_buf(),
                })
            }
        }
    }

    /// The lock file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Compute the canonical lock file path: `<config_dir>/gh-monitor.lock`.
/// We deliberately share a file with `config.toml`'s parent dir so the
/// lock survives a config-file move (rare, but possible if the user
/// uses `XDG_CONFIG_HOME` overrides or symlinks).
pub fn lock_path() -> PathBuf {
    use crate::config_io::config_path;
    config_path()
        .parent()
        .map(|p| p.join("gh-monitor.lock"))
        .unwrap_or_else(|| PathBuf::from("gh-monitor.lock"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    /// Build a unique temp lock path. We use `std::env::temp_dir()` plus
    /// an atomic counter and the process id so parallel tests don't
    /// collide. Tests clean up the leaf file.
    fn temp_lock_path(name: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "gh-monitor-single-instance-test-{}-{}-{}",
            std::process::id(),
            n,
            name
        ))
    }

    fn cleanup(path: &Path) {
        let _ = std::fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir(parent);
        }
    }

    #[test]
    fn second_acquire_fails_while_first_holds() {
        let path = temp_lock_path("held");
        let first = SingleInstance::new(&path).expect("first lock should succeed");
        let second = SingleInstance::new(&path);
        assert!(
            second.is_err(),
            "second acquire must fail while first is held"
        );
        let err = second.unwrap_err();
        assert_eq!(err.path, path);
        assert!(
            err.to_string().contains("another instance"),
            "error message should mention another instance, got: {err}"
        );
        assert!(
            err.to_string().contains("gh-monitor.lock")
                || err
                    .to_string()
                    .contains(path.display().to_string().as_str()),
            "error message should include the path, got: {err}"
        );
        drop(first);
        cleanup(&path);
    }

    #[test]
    fn second_acquire_succeeds_after_first_dropped() {
        let path = temp_lock_path("drop");
        let first = SingleInstance::new(&path).expect("first lock should succeed");
        drop(first);
        let second = SingleInstance::new(&path);
        assert!(
            second.is_ok(),
            "second lock should succeed after first is dropped, got {second:?}"
        );
        cleanup(&path);
    }

    #[test]
    fn path_is_recorded() {
        let path = temp_lock_path("path");
        let guard = SingleInstance::new(&path).expect("lock should succeed");
        assert_eq!(guard.path(), path.as_path());
        cleanup(&path);
    }
}
