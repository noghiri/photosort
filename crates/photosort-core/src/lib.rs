//! Pure logic for photosort: metadata parsing, shot grouping, identity,
//! copy planning, lens rules and persisted state types.
//!
//! This crate performs no filesystem, thread, environment or UI work.
//! Callers pass bytes and data in and get data out, which keeps every
//! decision testable without a card, a disk or a GPU.
#![forbid(unsafe_code)]
// Parsers handle untrusted bytes; every access must be bounds-checked.
#![deny(clippy::indexing_slicing)]
#![cfg_attr(test, allow(clippy::indexing_slicing))]

pub mod identity;
pub mod metadata;
pub mod model;
