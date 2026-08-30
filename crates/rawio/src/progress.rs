//! The progress line. Everything that decides what it says is a pure function;
//! the writer only decides when to say it.

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use rawio_core::progress::Progress;

/// Redrawing faster than this buys nothing and costs a syscall each time.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

pub fn human_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 3] = [("GiB", 1 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

pub fn human_rate(bytes_per_second: f64) -> String {
    format!("{}/s", human_size(bytes_per_second.max(0.0) as u64))
}

/// Time left at the average rate so far, or `--` when there is nothing to go on.
pub fn eta(remaining: u64, bytes_per_second: f64) -> String {
    if remaining == 0 {
        return "0s".to_string();
    }
    if bytes_per_second <= 0.0 {
        return "--".to_string();
    }
    let seconds = (remaining as f64 / bytes_per_second).round() as u64;
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, (seconds % 3600) / 60),
    }
}

pub fn progress_line(label: &str, done: u64, total: u64, elapsed: Duration) -> String {
    let percent = (done * 100).checked_div(total).unwrap_or(100);
    let seconds = elapsed.as_secs_f64();
    let rate = if seconds > 0.0 {
        done as f64 / seconds
    } else {
        0.0
    };
    format!(
        "{label} {} / {}  {percent:>3}%  {}  {}",
        human_size(done),
        human_size(total),
        human_rate(rate),
        eta(total.saturating_sub(done), rate),
    )
}

/// Draws to stderr, leaving stdout for the result a script reads.
pub struct Bar {
    label: &'static str,
    started: Instant,
    last_drawn: Instant,
    width: usize,
    /// Last reported position, so a wait can redraw the line it interrupts.
    at: (u64, u64),
}

impl Bar {
    /// On by default only where someone is watching: a piped or redirected
    /// stderr gets nothing.
    pub fn enabled(disabled_by_flag: bool) -> bool {
        !disabled_by_flag && std::io::stderr().is_terminal()
    }

    pub fn new(label: &'static str) -> Self {
        let now = Instant::now();
        Self {
            label,
            started: now,
            last_drawn: now - MIN_INTERVAL,
            width: 0,
            at: (0, 0),
        }
    }

    fn draw(&mut self, line: &str) {
        let mut err = std::io::stderr().lock();
        let padding = self.width.saturating_sub(line.chars().count());
        let _ = write!(err, "\r{line}{:padding$}", "");
        let _ = err.flush();
        self.width = line.chars().count();
    }
}

impl Progress for Bar {
    fn advance(&mut self, done: u64, total: u64) {
        self.at = (done, total);
        if self.last_drawn.elapsed() < MIN_INTERVAL {
            return;
        }
        self.last_drawn = Instant::now();
        let line = progress_line(self.label, done, total, self.started.elapsed());
        self.draw(&line);
    }

    fn waiting(&mut self, what: &str) {
        let (done, total) = self.at;
        let line = progress_line(self.label, done, total, self.started.elapsed());
        self.draw(&format!("{line}  {what}"));
    }

    fn finish(&mut self, done: u64) {
        let line = progress_line(self.label, done, done, self.started.elapsed());
        self.draw(&line);
        let _ = writeln!(std::io::stderr().lock());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_step_through_the_binary_units() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(2 << 20), "2.0 MiB");
        assert_eq!(human_size(3 << 30), "3.0 GiB");
    }

    #[test]
    fn rates_are_per_second_in_the_same_units() {
        assert_eq!(human_rate(0.0), "0 B/s");
        assert_eq!(human_rate(1536.0), "1.5 KiB/s");
        assert_eq!(human_rate(18.2 * (1 << 20) as f64), "18.2 MiB/s");
    }

    #[test]
    fn eta_is_coarse_and_says_so_when_it_cannot_tell() {
        assert_eq!(eta(0, 100.0), "0s");
        assert_eq!(eta(500, 100.0), "5s");
        assert_eq!(eta(125_000, 1000.0), "2m 05s");
        assert_eq!(eta(3_720_000, 1000.0), "1h 02m");
        assert_eq!(eta(500, 0.0), "--");
    }

    #[test]
    fn the_line_carries_the_share_done_the_rate_and_the_time_left() {
        let line = progress_line("flash", 32 << 20, 512 << 20, Duration::from_secs(2));

        assert!(line.starts_with("flash "), "{line}");
        assert!(line.contains("32.0 MiB / 512.0 MiB"), "{line}");
        assert!(line.contains("6%"), "{line}");
        assert!(line.contains("16.0 MiB/s"), "{line}");
        assert!(line.contains("30s"), "{line}");
    }

    /// A finished transfer must not read as 99% because of rounding.
    #[test]
    fn a_complete_transfer_reads_as_a_hundred_percent() {
        let line = progress_line("dump", 1000, 1000, Duration::from_secs(1));
        assert!(line.contains("100%"), "{line}");
    }

    #[test]
    fn an_empty_transfer_does_not_divide_by_zero() {
        let line = progress_line("dump", 0, 0, Duration::from_secs(0));
        assert!(line.contains("100%"), "{line}");
    }
}
