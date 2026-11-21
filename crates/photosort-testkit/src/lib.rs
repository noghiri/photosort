//! Dev-only builders for synthetic JPEG, NEF and TIFF bytes.
//!
//! The builders write bytes from the format specifications and do not
//! depend on `photosort-core`, so a wrong assumption in the parser cannot
//! be mirrored into the fixtures.

pub mod files;
pub mod spec;
mod tiff_writer;

pub use files::*;
pub use spec::*;
/// Builds arbitrary TIFF layouts for tests that need a shape the fixed
/// builders do not produce, such as several candidate JPEGs.
pub use tiff_writer::{IfdBuilder, write_tiff};
