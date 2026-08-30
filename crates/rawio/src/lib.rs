//! CLI surface. Exposed as a library so the command paths can be driven by a
//! fake backend in tests without spawning a process.

pub mod app;
pub mod cli;
pub mod longpath;
