//! Read-only PIT interpretation, opt-in.
//!
//! Layout is from reverse-engineered documentation, not a vendor spec. Two
//! independent sources agree on it: an XDA structure analysis and the Kaitai
//! Struct definition at github.com/CruelKernel/samsung_pit.
//!
//! Header, 28 bytes, in order: magic (4), entry count (4), port (4, ASCII),
//! format (4, ASCII), chip (8, ASCII), one unidentified dword (4). Then
//! `entry count` entries of 132 bytes each.
//!
//! Field offsets within an entry: binary type 0, device type 4, identifier 8,
//! attributes 12, update attributes 16, block offset 20, block count 24, two
//! obsolete dwords at 28 and 32, partition name 36 (32B), flash filename 68
//! (32B), FOTA filename 100 (32B).
//!
//! All little endian; all names are NUL padded.
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

/// Storage the entry describes. An SD card is expected to report `Mmc`, which
/// is the closest thing to evidence that the 512B block assumption holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    OneNand,
    FileFat,
    Mmc,
    All,
    Unknown(u32),
}

impl DeviceType {
    fn from_raw(raw: u32) -> Self {
        match raw {
            0 => DeviceType::OneNand,
            1 => DeviceType::FileFat,
            2 => DeviceType::Mmc,
            3 => DeviceType::All,
            other => DeviceType::Unknown(other),
        }
    }
}

impl std::fmt::Display for DeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceType::OneNand => f.write_str("onenand"),
            DeviceType::FileFat => f.write_str("filefat"),
            DeviceType::Mmc => f.write_str("mmc"),
            DeviceType::All => f.write_str("all"),
            DeviceType::Unknown(raw) => write!(f, "unknown({raw})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub name: String,
    pub flash_filename: String,
    pub identifier: u32,
    pub device_type: DeviceType,
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
    /// Header strings, printed so an implausible parse is visible to the user.
    pub port: String,
    pub format: String,
    pub chip: String,
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
            port: ascii_at(bytes, 8, 4),
            format: ascii_at(bytes, 12, 4),
            chip: ascii_at(bytes, 16, 8),
            partitions,
        })
    }

    /// Identifiers are the other addressable column of the table.
    pub fn find_by_id(&self, id: u32) -> Result<&Partition> {
        self.partitions
            .iter()
            .find(|p| p.identifier == id)
            .ok_or_else(|| {
                let ids = self
                    .partitions
                    .iter()
                    .map(|p| format!("{}={}", p.identifier, p.name))
                    .collect::<Vec<_>>()
                    .join(", ");
                Error::Pit(format!("no partition with id {id}; the table has: {ids}"))
            })
    }

    /// The name has to come from the table, so a miss reports what is there.
    pub fn find(&self, name: &str) -> Result<&Partition> {
        self.partitions
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| {
                let names = self
                    .partitions
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                Error::Pit(format!(
                    "no partition named {name:?}; the table has: {names}"
                ))
            })
    }
}

fn parse_entry(entry: &[u8], index: usize) -> Result<Partition> {
    let name = name_at(entry, 36)
        .ok_or_else(|| Error::Pit(format!("entry {index} has a non-UTF-8 name")))?;
    if name.is_empty() {
        return Err(Error::Pit(format!("entry {index} has an empty name")));
    }
    Ok(Partition {
        name,
        flash_filename: name_at(entry, 68).unwrap_or_default(),
        identifier: u32_at(entry, 8),
        device_type: DeviceType::from_raw(u32_at(entry, 4)),
        block_offset: u32_at(entry, 20),
        block_count: u32_at(entry, 24),
    })
}

/// NUL padded 32B name field.
fn name_at(entry: &[u8], at: usize) -> Option<String> {
    let raw = &entry[at..at + 32];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    Some(std::str::from_utf8(&raw[..end]).ok()?.trim().to_string())
}

/// Header strings are fixed width and NUL padded, and may be blank.
fn ascii_at(bytes: &[u8], at: usize, len: usize) -> String {
    let raw = &bytes[at..at + len];
    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).trim().to_string()
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
        buf[8..12].copy_from_slice(b"COM4");
        buf[12..16].copy_from_slice(b"FILE");
        buf[16..22].copy_from_slice(b"EMMC16");
        for (i, (name, offset, count)) in entries.iter().enumerate() {
            let entry = &mut buf[HEADER_LEN + i * ENTRY_LEN..][..ENTRY_LEN];
            entry[4..8].copy_from_slice(&2u32.to_le_bytes()); // device type: mmc
            entry[8..12].copy_from_slice(&(i as u32).to_le_bytes());
            entry[20..24].copy_from_slice(&offset.to_le_bytes());
            entry[24..28].copy_from_slice(&count.to_le_bytes());
            entry[36..36 + name.len()].copy_from_slice(name.as_bytes());
        }
        buf
    }

    #[test]
    fn parses_entries_and_resolves_byte_ranges() {
        let pit = Pit::parse(&build(&[("BOOT", 2048, 128), ("LOG", 8192, 1024)])).unwrap();

        assert_eq!(pit.port, "COM4");
        assert_eq!(pit.format, "FILE");
        assert_eq!(pit.chip, "EMMC16");
        assert_eq!(pit.partitions.len(), 2);
        let log = pit.find("log").unwrap();
        assert_eq!(log.device_type, DeviceType::Mmc);
        assert_eq!(log.identifier, 1);
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
    fn a_partition_is_addressable_by_identifier() {
        let pit = Pit::parse(&build(&[("BOOT", 0, 1), ("LOG", 8, 2)])).unwrap();

        assert_eq!(pit.find_by_id(1).unwrap().name, "LOG");
        assert_eq!(pit.find_by_id(1).unwrap(), pit.find("LOG").unwrap());
    }

    #[test]
    fn an_unknown_identifier_reports_the_ones_that_exist() {
        let pit = Pit::parse(&build(&[("BOOT", 0, 1), ("LOG", 8, 2)])).unwrap();

        let message = pit.find_by_id(9).unwrap_err().to_string();

        assert!(
            message.contains("0=BOOT") && message.contains("1=LOG"),
            "{message}"
        );
    }

    /// The name has to come from somewhere, and the table is the only source.
    #[test]
    fn an_unknown_name_reports_the_names_that_exist() {
        let pit = Pit::parse(&build(&[("BOOT", 0, 1), ("LOG", 1, 1)])).unwrap();

        let message = pit.find("nope").unwrap_err().to_string();

        assert!(
            message.contains("BOOT") && message.contains("LOG"),
            "{message}"
        );
    }
}
