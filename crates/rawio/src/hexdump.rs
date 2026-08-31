//! The hexdump. Byte for byte what `hexdump -C` prints, so its output diffs
//! against the tool every reader already has.

use std::io::{self, Write};

/// Bytes per line, as `hexdump -C` groups them.
const LINE: usize = 16;

/// Width of the byte columns, including the gap between the two groups of
/// eight. A short last line is padded to it so the character column does not
/// move.
const FIELD: usize = LINE * 3 + 1;

/// Offsets are printed 8 wide until the device needs more, and then 16, so
/// every line of one dump has the same width.
pub fn offset_width(end: u64) -> usize {
    if end > u64::from(u32::MAX) { 16 } else { 8 }
}

/// One output line: the offset it starts at, up to 16 bytes, and the printable
/// characters those bytes stand for.
pub fn line(offset: u64, width: usize, bytes: &[u8]) -> String {
    let mut field = String::with_capacity(FIELD);
    for (i, byte) in bytes.iter().enumerate() {
        if i == LINE / 2 {
            field.push(' ');
        }
        field.push_str(&format!("{byte:02x} "));
    }

    let mut text = format!("{offset:0width$x}  {field:<FIELD$} |");
    text.extend(bytes.iter().map(|byte| printable(*byte)));
    text.push('|');
    text
}

/// Everything outside printable ASCII is a dot: the high half is what the
/// terminals disagree about.
fn printable(byte: u8) -> char {
    match byte {
        0x20..=0x7e => byte as char,
        _ => '.',
    }
}

/// Formats a stream of bytes, which arrive in whatever sizes the device was
/// read in, into lines of 16.
pub struct Hexdump {
    width: usize,
    squeeze: bool,
    /// Offset of the first byte still in `pending`.
    at: u64,
    pending: Vec<u8>,
    previous: Option<Vec<u8>>,
    /// A run of identical lines is already reported; nothing more to print
    /// until one differs.
    collapsed: bool,
}

impl Hexdump {
    pub fn new(start: u64, end: u64, squeeze: bool) -> Self {
        Self {
            width: offset_width(end),
            squeeze,
            at: start,
            pending: Vec::with_capacity(LINE),
            previous: None,
            collapsed: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8], out: &mut dyn Write) -> io::Result<()> {
        for byte in bytes {
            self.pending.push(*byte);
            if self.pending.len() == LINE {
                self.emit(out)?;
            }
        }
        Ok(())
    }

    /// Prints the full line that has accumulated, or the one `*` that stands
    /// for the run of identical lines it continues.
    fn emit(&mut self, out: &mut dyn Write) -> io::Result<()> {
        let repeat = self.squeeze && self.previous.as_deref() == Some(self.pending.as_slice());
        if !repeat {
            writeln!(out, "{}", line(self.at, self.width, &self.pending))?;
        } else if !self.collapsed {
            writeln!(out, "*")?;
        }
        self.collapsed = repeat;
        self.at += self.pending.len() as u64;
        self.previous = Some(std::mem::replace(
            &mut self.pending,
            Vec::with_capacity(LINE),
        ));
        Ok(())
    }

    /// Prints the part line left over and the end offset, which is how
    /// `hexdump` says where the dump stopped.
    pub fn finish(&mut self, out: &mut dyn Write) -> io::Result<()> {
        if !self.pending.is_empty() {
            writeln!(out, "{}", line(self.at, self.width, &self.pending))?;
            self.at += self.pending.len() as u64;
            self.pending.clear();
        }
        let width = self.width;
        writeln!(out, "{:0width$x}", self.at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(start: u64, data: &[u8], squeeze: bool, chunk: usize) -> String {
        let mut out = Vec::new();
        let mut dump = Hexdump::new(start, start + data.len() as u64, squeeze);
        for piece in data.chunks(chunk) {
            dump.push(piece, &mut out).unwrap();
        }
        dump.finish(&mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The output is only worth anything if it is the output every reader
    /// already knows; this is `hexdump -C` of the same bytes.
    #[test]
    fn a_line_is_what_hexdump_c_prints() {
        let bytes = b"\xeb\x3c\x90MSDOS5.0\x00\x02\x08\x20\x00";

        assert_eq!(
            line(0, 8, bytes),
            "00000000  eb 3c 90 4d 53 44 4f 53  35 2e 30 00 02 08 20 00  |.<.MSDOS5.0... .|"
        );
    }

    /// The character column has to stay where it was, or a short last line
    /// makes the whole dump unreadable.
    #[test]
    fn a_short_line_keeps_the_character_column() {
        let full = line(0, 8, &[0u8; LINE]);
        let short = line(0x50, 8, b"abc");

        assert_eq!(short.find('|'), full.find('|'));
        assert!(short.ends_with("|abc|"), "{short}");
    }

    /// Anything outside printable ASCII is a dot, including the high half no
    /// terminal agrees on.
    #[test]
    fn only_printable_ascii_reaches_the_character_column() {
        let bytes: Vec<u8> = vec![0x00, 0x1f, 0x20, 0x7e, 0x7f, 0x80, 0xff, b'A'];
        let rendered = line(0, 8, &bytes);

        assert!(rendered.ends_with("|.. ~...A|"), "{rendered}");
    }

    #[test]
    fn the_last_line_is_the_offset_the_dump_stopped_at() {
        let out = render(0x50, b"abc", true, 16);

        assert_eq!(
            out,
            "00000050  61 62 63                                          |abc|\n00000053\n"
        );
    }

    /// A card is mostly one repeated byte; printing every line of it hides the
    /// three that matter.
    #[test]
    fn a_run_of_identical_lines_collapses_to_a_star() {
        let mut data = vec![0u8; LINE * 4];
        data[LINE * 3] = 0xff;

        let out = render(0, &data, true, 16);
        let lines: Vec<&str> = out.lines().collect();

        assert_eq!(lines.len(), 4, "{out}");
        assert!(lines[0].starts_with("00000000  00 00"), "{out}");
        assert_eq!(lines[1], "*");
        assert!(lines[2].starts_with("00000030  ff 00"), "{out}");
        assert_eq!(lines[3], "00000040");
    }

    #[test]
    fn nothing_collapses_when_the_squeeze_is_off() {
        let out = render(0, &[0u8; LINE * 4], false, 16);

        assert_eq!(out.lines().count(), 5, "{out}");
        assert!(!out.contains('*'), "{out}");
    }

    /// The device hands over whatever the read returned, which has nothing to
    /// do with 16.
    #[test]
    fn chunks_that_do_not_land_on_a_line_boundary_still_print_whole_lines() {
        let data: Vec<u8> = (0..100u8).collect();

        let whole = render(0, &data, true, 100);
        for chunk in [1, 3, 7, 16, 33] {
            assert_eq!(render(0, &data, true, chunk), whole, "chunk {chunk}");
        }
    }

    /// A dump that crosses four gibibytes must not change column halfway.
    #[test]
    fn the_offset_column_is_wide_enough_for_the_whole_dump() {
        assert_eq!(offset_width(0), 8);
        assert_eq!(offset_width(u64::from(u32::MAX)), 8);
        assert_eq!(offset_width(u64::from(u32::MAX) + 1), 16);

        let out = render(0xffff_fff0, &[0u8; LINE * 2], true, 32);
        let widths: Vec<usize> = out
            .lines()
            .filter(|l| *l != "*")
            .map(|l| l.split_whitespace().next().unwrap().len())
            .collect();

        assert!(widths.iter().all(|w| *w == 16), "{out}");
    }
}
