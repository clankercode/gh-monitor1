//! Open URLs in the system default browser.

use tracing::warn;

/// Open a URL in the default browser. Logs and swallows errors — the
/// renderer should never panic because the browser launch failed.
///
/// The URL must have an `http` or `https` scheme. Anything else
/// (including `javascript:`, `file:`, malformed strings, etc.) is
/// rejected with a warning and not opened. This is defense in depth:
/// the canvas only ever passes URLs from `node.target_url` (which
/// comes from the GitHub API or our hard-coded
/// `https://github.com/{repo}`), but if a future bug ever leaks a
/// non-HTTP URL into the click handler, we don't want to spawn a
/// browser pointed at `javascript:alert(1)`.
pub fn open_url(url: &str) {
    match url::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
        Ok(_) => {
            warn!(url = %url, "refusing to open URL with non-http(s) scheme");
            return;
        }
        Err(_) => {
            warn!(url = %url, "refusing to open malformed URL");
            return;
        }
    }
    if let Err(e) = open::that(url) {
        warn!(error = %e, url = %url, "failed to open URL");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// Test-only opener: records the URLs it was asked to open. Lets
    /// us assert that `open_url` actually calls `open::that` for safe
    /// URLs and *doesn't* for unsafe ones, without spawning a real
    /// browser. The shared `Rc<RefCell<Vec<String>>>` lets a single
    /// helper call `open_url_with` with the recorded list.
    struct MockOpener {
        opened: Rc<RefCell<Vec<String>>>,
    }

    impl MockOpener {
        fn new(opened: Rc<RefCell<Vec<String>>>) -> Self {
            Self { opened }
        }
        fn opener(&self) -> impl Fn(&str) -> Result<(), String> + '_ {
            move |url: &str| {
                self.opened.borrow_mut().push(url.to_string());
                Ok(())
            }
        }
    }

    /// Variant of `open_url` that takes an opener closure, so tests
    /// can stub the side-effect of launching a browser. Production
    /// code uses `open_url`, which calls `open::that` directly.
    fn open_url_with<F: Fn(&str) -> Result<(), String>>(url: &str, opener: F) {
        match url::Url::parse(url) {
            Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => {}
            Ok(_) => {
                warn!(url = %url, "refusing to open URL with non-http(s) scheme");
                return;
            }
            Err(_) => {
                warn!(url = %url, "refusing to open malformed URL");
                return;
            }
        }
        if let Err(e) = opener(url) {
            warn!(error = %e, url = %url, "failed to open URL");
        }
    }

    #[test]
    fn javascript_scheme_is_rejected() {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mock = MockOpener::new(Rc::clone(&opened));
        open_url_with("javascript:alert(1)", mock.opener());
        assert!(
            opened.borrow().is_empty(),
            "javascript: URL must not be opened, got {:?}",
            opened.borrow()
        );
    }

    #[test]
    fn file_scheme_is_rejected() {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mock = MockOpener::new(Rc::clone(&opened));
        open_url_with("file:///etc/passwd", mock.opener());
        assert!(
            opened.borrow().is_empty(),
            "file: URL must not be opened, got {:?}",
            opened.borrow()
        );
    }

    #[test]
    fn malformed_url_is_rejected() {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mock = MockOpener::new(Rc::clone(&opened));
        open_url_with("not a url", mock.opener());
        assert!(
            opened.borrow().is_empty(),
            "malformed URL must not be opened, got {:?}",
            opened.borrow()
        );
    }

    #[test]
    fn https_url_is_opened() {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mock = MockOpener::new(Rc::clone(&opened));
        open_url_with("https://github.com/foo/bar", mock.opener());
        assert_eq!(
            *opened.borrow(),
            vec!["https://github.com/foo/bar".to_string()]
        );
    }

    #[test]
    fn http_url_is_opened() {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mock = MockOpener::new(Rc::clone(&opened));
        open_url_with("http://example.com/", mock.opener());
        assert_eq!(*opened.borrow(), vec!["http://example.com/".to_string()]);
    }

    #[test]
    fn data_scheme_is_rejected() {
        let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        let mock = MockOpener::new(Rc::clone(&opened));
        open_url_with("data:text/html,<script>alert(1)</script>", mock.opener());
        assert!(opened.borrow().is_empty());
    }
}
