//! Read-only PIT interpretation, opt-in.
//!
//! Layout is from reverse-engineered documentation, not a vendor spec:
//! header 28B = magic(4) + entry count(4) + 4 unused dwords(16) + LUN count(4),
//! followed by 132B entries. Field offsets within an entry: block offset at 20,
//! block count at 24, name at 36 (32B, NUL padded). All little endian.
//!
//! A plausible-looking but wrong interpretation is not detectable here, so the
//! caller must print the resolved range before acting on it.

use crate::error::{Error, Result};

pub const MAGIC: u32 = 0x1234_9876;
pub const HEADER_LEN: usize = 28;
pub const ENTRY_LEN: usize = 132;

/// The unit of `block_offset`/`block_count` is undocumented. 512 is an assumption;
/// comparing a resolved range against an explicit-offset run is what disproves it.
pub const ASSUMED_BLOCK_SIZE: u64 = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub block_offset: u32,
    pub block_count: u32,
}

impl Partition {
    pub fn byte_offset(&self) -> u64 {
        u64::from(self.block_offset) * ASSUMED_BLOCK_SIZE
    }

    pub fn byte_length(&self) -> u64 {
        u64::from(self.block_count) * ASSUMED_BLOCK_SIZE
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pit {
    pub lun_count: u32,
    pub partitions: Vec<Partition>,
}

impl Pit {
    /// Any failure here aborts before the device is read or written.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_LEN {
            return Err(Error::Pit(format!(
                "header truncated: {} bytes, need {HEADER_LEN}",
                bytes.len()
            )));
        }
        let magic = u32_at(bytes, 0);
        if magic != MAGIC {
            return Err(Error::Pit(format!(
                "bad magic {magic:#010x}, expected {MAGIC:#010x}"
            )));
        }

        let entry_count = u32_at(bytes, 4) as usize;
        let lun_count = u32_at(bytes, 24);
        let needed = HEADER_LEN + entry_count * ENTRY_LEN;
        if bytes.len() < needed {
            return Err(Error::Pit(format!(
                "{entry_count} entries need {needed} bytes, got {}",
                bytes.len()
            )));
        }

        let partitions = (0..entry_count)
            .map(|i| parse_entry(&bytes[HEADER_LEN + i * ENTRY_LEN..][..ENTRY_LEN], i))
            .collect::<Result<Vec<_>>>()?;

        Ok(Self {
            lun_count,
            partitions,
        })
    }

    pub fn find(&self, name: &str) -> Result<&Partition> {
        self.partitions
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| Error::Pit(format!("no partition named {name:?}")))
    }
}

fn parse_entry(entry: &[u8], index: usize) -> Result<Partition> {
    let raw = &entry[36..68];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    let name = std::str::from_utf8(&raw[..end])
        .map_err(|_| Error::Pit(format!("entry {index} has a non-UTF-8 name")))?
        .trim()
        .to_string();
    if name.is_empty() {
        return Err(Error::Pit(format!("entry {index} has an empty name")));
    }
    Ok(Partition {
        name,
        block_offset: u32_at(entry, 20),
        block_count: u32_at(entry, 24),
    })
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(
        bytes[at..at + 4]
            .try_into()
            .expect("caller checked the length"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(entries: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut buf = vec![0u8; HEADER_LEN + entries.len() * ENTRY_LEN];
        buf[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        buf[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        buf[24..28].copy_from_slice(&1u32.to_le_bytes());
        for (i, (name, offset, count)) in entries.iter().enumerate() {
            let entry = &mut buf[HEADER_LEN + i * ENTRY_LEN..][..ENTRY_LEN];
            entry[20..24].copy_from_slice(&offset.to_le_bytes());
            entry[24..28].copy_from_slice(&count.to_le_bytes());
            entry[36..36 + name.len()].copy_from_slice(name.as_bytes());
        }
        buf
    }

    #[test]
    fn parses_entries_and_resolves_byte_ranges() {
        let pit = Pit::parse(&build(&[("BOOT", 2048, 128), ("LOG", 8192, 1024)])).unwrap();

        assert_eq!(pit.lun_count, 1);
        assert_eq!(pit.partitions.len(), 2);
        let log = pit.find("log").unwrap();
        assert_eq!(log.byte_offset(), 8192 * 512);
        assert_eq!(log.byte_length(), 1024 * 512);
    }

    #[test]
    fn rejects_a_missing_magic() {
        let mut bytes = build(&[("BOOT", 0, 1)]);
        bytes[0] ^= 0xFF;
        assert!(matches!(Pit::parse(&bytes), Err(Error::Pit(_))));
    }

    #[test]
    fn rejects_a_truncated_entry_table() {
        let mut bytes = build(&[("BOOT", 0, 1), ("LOG", 1, 1)]);
        bytes.truncate(HEADER_LEN + ENTRY_LEN);
        assert!(matches!(Pit::parse(&bytes), Err(Error::Pit(_))));
    }

    #[test]
    fn unknown_partition_name_is_an_error() {
        let pit = Pit::parse(&build(&[("BOOT", 0, 1)])).unwrap();
        assert!(pit.find("nope").is_err());
    }
}
