//! Open URLs in the system default browser.

use tracing::warn;

/// Open a URL in the default browser. Logs and swallows errors — the
/// renderer should never panic because the browser launch failed.
pub fn open_url(url: &str) {
    if let Err(e) = open::that(url) {
        warn!(error = %e, url = %url, "failed to open URL");
    }
}
