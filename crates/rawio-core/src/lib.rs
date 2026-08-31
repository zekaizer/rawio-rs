//! Device access and transfer logic. No terminal output and no argument parsing
//! live here - the CLI crate owns both.

pub mod device;
pub mod error;
pub mod parts;
pub mod pit;
pub mod platform;
pub mod progress;
pub mod trace;
pub mod transfer;
