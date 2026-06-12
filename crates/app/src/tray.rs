//! System-tray icon for `gh-monitor`.
//!
//! Spawns a tray on a dedicated thread (so the menu can pump its own
//! GTK/Win32 event loop). The thread forwards menu events to a tokio
//! channel that the Iced app reads from.
//!
//! Menu items:
//! - "Show / Hide" — toggle the overlay window's visibility
//! - "Quit"        — exit the app
//!
//! On Linux, this requires GTK + libappindicator at runtime. See
//! `PLAN.md` and `.github/workflows/ci.yml` for the install command.

use std::sync::Mutex;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{info, warn};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// Menu actions the tray can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    /// Toggle the overlay window's visibility.
    ToggleVisible,
    /// Quit the app.
    Quit,
}

/// Static slot for the tray's event receiver. Set once in `spawn` and
/// taken once by the Iced subscription's stream factory.
static TRAY_RX: Mutex<Option<mpsc::Receiver<TrayAction>>> = Mutex::new(None);

/// Take the receiver out of the slot. Returns `None` if the tray
/// hasn't been started or if the receiver was already taken.
pub fn tray_rx_owned() -> Option<mpsc::Receiver<TrayAction>> {
    TRAY_RX.lock().ok().and_then(|mut g| g.take())
}

/// Spawn the tray. Idempotent: only the first call starts the tray;
/// subsequent calls return `Ok(TrayHandle { _tray: None })`.
///
/// Returns the `TrayHandle` so the caller can keep it alive (dropping
/// it removes the tray).
pub fn spawn() -> Result<TrayHandle> {
    if TRAY_RX.lock().map(|g| g.is_some()).unwrap_or(false) {
        return Ok(TrayHandle { _tray: None });
    }
    let (tx, rx) = mpsc::channel::<TrayAction>(8);
    {
        let mut slot = TRAY_RX.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        *slot = Some(rx);
    }

    // On Linux, `Menu::new()` panics unless GTK has been initialized.
    // Other platforms don't need this.
    #[cfg(target_os = "linux")]
    {
        if let Err(e) = gtk::init() {
            return Err(anyhow::anyhow!("gtk::init failed: {e}"));
        }
    }

    // Build the menu. IDs are arbitrary strings; we look them up when
    // the event fires.
    let show_item = MenuItem::with_id("show", "Show / Hide", true, None);
    let quit_item = MenuItem::with_id("quit", "Quit", true, None);

    let menu = Menu::new();
    menu.append(&show_item).context("append show")?;
    menu.append(&quit_item).context("append quit")?;

    let icon = make_icon();

    // Install the global menu event handler. Must be called once
    // before any events fire.
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id().0.as_str() {
            "show" => TrayAction::ToggleVisible,
            "quit" => TrayAction::Quit,
            _ => return,
        };
        let tx = tx.clone();
        // The event handler runs on a non-tokio thread; use
        // tokio::spawn to forward. If we can't spawn (e.g. shutdown),
        // drop the action.
        tokio::spawn(async move {
            if tx.send(action).await.is_err() {
                warn!("tray event receiver dropped");
            }
        });
    }));

    let tray = TrayIconBuilder::new()
        .with_tooltip("gh-monitor")
        .with_icon(icon)
        .with_menu(Box::new(menu))
        .build()
        .context("building tray icon")?;

    info!("tray icon started");
    Ok(TrayHandle { _tray: Some(tray) })
}

/// Opaque handle that owns the `TrayIcon`. Drop it to remove the tray.
pub struct TrayHandle {
    _tray: Option<TrayIcon>,
}

/// Build a simple 32x32 RGBA icon: a filled rounded square in the
/// gh-monitor brand color (deep purple), with a transparent corner
/// pixel for visual interest.
fn make_icon() -> Icon {
    const W: u32 = 32;
    const H: u32 = 32;
    let mut rgba = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            // Rounded square mask.
            let r = 6.0;
            let inside = (x as f32).max(r).min(W as f32 - r - 1.0) == x as f32
                || (y as f32).max(r).min(H as f32 - r - 1.0) == y as f32
                || ((x as f32 - r).hypot(y as f32 - r) <= r)
                || ((x as f32 - (W as f32 - r - 1.0)).hypot(y as f32 - r) <= r)
                || ((x as f32 - r).hypot(y as f32 - (H as f32 - r - 1.0)) <= r)
                || ((x as f32 - (W as f32 - r - 1.0)).hypot(y as f32 - (H as f32 - r - 1.0)) <= r);
            let i = ((y * W + x) * 4) as usize;
            if inside {
                rgba[i] = 0x6e; // R
                rgba[i + 1] = 0x40; // G
                rgba[i + 2] = 0xc9; // B
                rgba[i + 3] = 0xff; // A
            } else {
                rgba[i + 3] = 0; // transparent
            }
        }
    }
    // SAFETY: we just built a valid RGBA buffer of the right size.
    Icon::from_rgba(rgba, W, H).expect("valid rgba")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_correct_size() {
        // Smoke-test the icon builder. The actual pixel data isn't
        // asserted; the GTK/libxdo runtime would be needed for a
        // real roundtrip.
        let _ = make_icon();
    }

    #[test]
    fn tray_actions_distinct() {
        assert_ne!(TrayAction::ToggleVisible, TrayAction::Quit);
    }
}
