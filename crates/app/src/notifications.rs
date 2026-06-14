//! Desktop notifications. One function: [`fire`].
//!
//! The overlay shells out to the platform's native notification
//! daemon — `notify-send` on Linux, `osascript` on macOS, and a
//! PowerShell `New-BurntToastNotification` on Windows. We avoid
//! the `notify-rust` crate here because its transitive
//! dependency set (zbus, winrt-notification,
//! mac-notification-sys) is large relative to the trivial
//! integration with the platform tool we actually need.
//!
//! In tests, [`fire`] is switched into a capture mode that
//! records each call into a thread-safe buffer instead of
//! spawning a process. Tests can drain the buffer with
//! [`take_captured`] and assert on the title/body the GUI would
//! have shown. This is the only production-grade way to verify
//! notification behaviour without depending on a desktop
//! environment, which CI does not provide.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tracing::warn;

/// One captured notification. Tests compare these to the
/// expected list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedNotification {
    pub title: String,
    pub body: String,
}

static CAPTURE_MODE: AtomicBool = AtomicBool::new(false);
static CAPTURED: Mutex<Vec<CapturedNotification>> = Mutex::new(Vec::new());

/// Switch the module into capture mode (true) or production mode
/// (false). In capture mode, [`fire`] records its arguments into
/// a global buffer instead of spawning a process. Production code
/// leaves this at its default of `false`. Tests flip it to `true`
/// for the duration of the test and back to `false` afterwards
/// (see [`TEST_LOCK`] for the serialiser used by the
/// `gh-monitor-app` tests).
pub fn set_capture_mode(enabled: bool) {
    CAPTURE_MODE.store(enabled, Ordering::SeqCst);
}

/// Drain the captured-notification buffer, leaving it empty.
/// Only meaningful in capture mode. Returns an empty vec in
/// production mode.
pub fn take_captured() -> Vec<CapturedNotification> {
    CAPTURED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .drain(..)
        .collect()
}

/// Fire a desktop notification with `title` and `body`. In
/// capture mode the call is recorded; in production mode the
/// platform's native notifier is spawned. Errors are logged at
/// WARN and never propagated — a failed notification must not
/// bring the overlay down.
pub fn fire(title: &str, body: &str) {
    if CAPTURE_MODE.load(Ordering::SeqCst) {
        if let Ok(mut g) = CAPTURED.lock() {
            g.push(CapturedNotification {
                title: title.to_string(),
                body: body.to_string(),
            });
        }
        return;
    }
    if let Err(e) = platform_send(title, body) {
        warn!(error = %e, title, "failed to fire desktop notification");
    }
}

#[cfg(target_os = "linux")]
fn platform_send(title: &str, body: &str) -> std::io::Result<()> {
    // `notify-send` is the freedesktop standard. Most distros
    // ship it; if missing the spawned process fails with ENOENT
    // and the overlay logs a WARN.
    std::process::Command::new("notify-send")
        .arg(title)
        .arg(body)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "macos")]
fn platform_send(title: &str, body: &str) -> std::io::Result<()> {
    // AppleScript's `display notification` uses the macOS
    // notification centre. The strings are passed as
    // AppleScript literals (with `?` debug-format quoting) so
    // any embedded double-quote in a repo name is escaped.
    let script = format!("display notification {:?} with title {:?}", body, title);
    std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .spawn()
        .map(|_| ())
}

#[cfg(target_os = "windows")]
fn platform_send(title: &str, body: &str) -> std::io::Result<()> {
    // BurntToast is the most common PowerShell notification
    // module. Users who do not have it will see a Windows
    // script host error; the WARN log captures it.
    let script = format!(
        "[reflection.assembly]::loadwithpartialname('BurntToast') | Out-Null; \
         New-BurntToastNotification -Text '{}', '{}'",
        title.replace('\'', "''"),
        body.replace('\'', "''"),
    );
    std::process::Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(script)
        .spawn()
        .map(|_| ())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn platform_send(_title: &str, _body: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "desktop notifications are not supported on this platform",
    ))
}

/// A mutex serialising tests that touch [`CAPTURE_MODE`] /
/// [`CAPTURED`]. Tests acquire this at the top of their body
/// so the parallel `cargo test` runner does not interleave
/// their capture and drain steps.
pub static TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_mode_records_calls() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_capture_mode(true);
        // Make sure we start from a clean buffer even if a
        // prior test forgot to drain.
        let _ = take_captured();
        fire("title-a", "body-a");
        fire("title-b", "body-b");
        let captured = take_captured();
        set_capture_mode(false);
        assert_eq!(
            captured,
            vec![
                CapturedNotification {
                    title: "title-a".to_string(),
                    body: "body-a".to_string(),
                },
                CapturedNotification {
                    title: "title-b".to_string(),
                    body: "body-b".to_string(),
                },
            ]
        );
    }

    #[test]
    fn take_captured_drains_buffer() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_capture_mode(true);
        let _ = take_captured();
        fire("a", "b");
        let once = take_captured();
        let twice = take_captured();
        set_capture_mode(false);
        assert_eq!(once.len(), 1);
        assert!(twice.is_empty());
    }
}
