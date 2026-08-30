//! Transfer progress. The core reports numbers; how they are drawn, and whether
//! they are drawn at all, belongs to the front end.

/// Called from inside the transfer loop after each chunk lands.
pub trait Progress {
    fn advance(&mut self, done: u64, total: u64);
    fn finish(&mut self, done: u64);
}

/// Progress nobody is watching.
pub struct Silent;

impl Progress for Silent {
    fn advance(&mut self, _done: u64, _total: u64) {}
    fn finish(&mut self, _done: u64) {}
}
