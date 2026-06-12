//! The `gh-monitor` binary: an Iced-based transparent overlay.

#![forbid(unsafe_code)]
#![allow(missing_docs)]
#![allow(dead_code)]

mod animation;
mod app;
mod canvas;
mod config_io;
mod link;
mod overlay;
mod paint;
mod tray;

pub use app::{run, AppSettings};
pub use config_io::load_config;
