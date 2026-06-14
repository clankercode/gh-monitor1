//! The `gh-monitor` binary: an Iced-based transparent overlay.

#![deny(unsafe_code)]
#![allow(missing_docs)]
#![allow(dead_code)]

mod animation;
mod app;
mod canvas;
pub mod config_io;
mod context_menu;
mod demo;
pub mod doctor;
mod link;
mod notifications;
mod overlay;
mod paint;
pub mod settings;
pub mod setup;
pub mod single_instance;
pub mod tray;

pub use app::{run, AppSettings};
pub use config_io::load_config;
