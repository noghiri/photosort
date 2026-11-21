//! Pure logic for photosort: metadata parsing, shot grouping, identity,
//! copy planning, lens rules and persisted state types.
//!
//! This crate performs no filesystem, thread, environment or UI work.
//! Callers pass bytes and data in and get data out, which keeps every
//! decision testable without a card, a disk or a GPU.
#![forbid(unsafe_code)]
