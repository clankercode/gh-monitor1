//! `gh-monitor-timeline` — group, compress, and humanize GitHub events.
//!
//! Pure logic. No async, no GUI. The heart of the product. Test it heavily.
//!
//! Modules:
//! - [`group`] — group events by repo
//! - [`compress`] — collapse similar events into `(type, count)` nodes with
//!   time ranges
//! - [`humanize`] — "1–3 hrs ago" formatting
//! - [`snapshot`] — point-in-time state used to diff for animations

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod compress;
pub mod group;
pub mod humanize;
pub mod snapshot;

pub use compress::{compress, CompressedNode, CompressionConfig};
pub use group::{group_by_repo, RepoGroup};
pub use humanize::humanize_range;
pub use snapshot::{diff, NodeId, NodeKind, TimelineNode, TimelineSnapshot};
