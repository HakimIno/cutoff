//! Domain layer for the video merger.
//!
//! This crate contains pure business rules with no I/O dependencies. It can
//! be unit-tested in isolation without spawning processes or touching disk.

pub mod domain;
pub mod errors;
pub mod services;

pub use errors::{CoreError, CoreResult};
