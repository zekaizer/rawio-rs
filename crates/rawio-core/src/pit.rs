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
use crate::parts::{Gap, MAX_READ_BYTES, Sectors};

pub const MAGIC: u32 = 0x1234_9876;
pub const HEADER_LEN: usize = 28;
pub const ENTRY_LEN: usize = 132;

/// Entry counts above this are treated as garbage rather than sized for; real
/// tables hold a few dozen entries.
pub const MAX_ENTRIES: usize = 4096;

/// How much unallocated space a search reads before giving up. The table sits
/// in front of the first partition on every card seen so far, and that gap is
/// searched backwards, so the default is reached only when it is somewhere
/// else entirely.
pub const DEFAULT_SCAN_BUDGET: u64 = 64 << 20;

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
    /// First phase of a two-phase read: proves everything a header-sized
    /// prefix can prove - length, magic, a plausible entry count - and returns
    /// the byte length of the whole table, so the caller never sizes an
    /// allocation or a device read from unvalidated bytes.
    pub fn table_len(head: &[u8]) -> Result<usize> {
        if head.len() < HEADER_LEN {
            return Err(Error::Pit(format!(
                "header truncated: {} bytes, need {HEADER_LEN}",
                head.len()
            )));
        }
        let magic = u32_at(head, 0);
        if magic != MAGIC {
            return Err(Error::Pit(format!(
                "bad magic {magic:#010x}, expected {MAGIC:#010x}"
            )));
        }
        let entry_count = u32_at(head, 4) as usize;
        if entry_count > MAX_ENTRIES {
            return Err(Error::Pit(format!(
                "{entry_count} entries is implausible (at most {MAX_ENTRIES} expected)"
            )));
        }
        Ok(HEADER_LEN + entry_count * ENTRY_LEN)
    }

    /// Any failure here aborts before the device is read or written.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let needed = Self::table_len(bytes)?;
        if bytes.len() < needed {
            return Err(Error::Pit(format!(
                "{} entries need {needed} bytes, got {}",
                (needed - HEADER_LEN) / ENTRY_LEN,
                bytes.len()
            )));
        }

        let partitions = (0..(needed - HEADER_LEN) / ENTRY_LEN)
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

/// A table and the offset it was found at. The offset is printed before
/// anything acts on a range, because a search that landed on the wrong copy is
/// only visible there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub pit: Pit,
    pub offset: u64,
}

/// Two-phase read at a known offset: the header is validated before its entry
/// count is allowed to size the read that follows it.
pub fn read_at(src: &mut dyn Sectors, offset: u64) -> Result<Pit> {
    let sector = u64::from(src.sector_size());
    if sector == 0 || offset % sector != 0 {
        return Err(Error::Pit(format!(
            "offset {offset} is not a multiple of the {sector}-byte sector size"
        )));
    }
    let lba = offset / sector;

    let head = src.read(lba, (HEADER_LEN as u64).div_ceil(sector).max(1))?;
    let needed = Pit::table_len(&head)?;
    if needed <= head.len() {
        return Pit::parse(&head);
    }

    let whole = src.read(lba, (needed as u64).div_ceil(sector))?;
    Pit::parse(&whole)
}

/// Searches the space no partition covers for the table's magic.
///
/// The magic alone proves nothing - four bytes come up by chance - so every hit
/// is parsed, and one that does not parse is passed over rather than reported.
/// `budget` caps the bytes read; `None` lifts the cap. A full pass over a large
/// card is bounded by read throughput, not by this loop, which is why the cap
/// exists.
pub fn scan(src: &mut dyn Sectors, gaps: &[Gap], budget: Option<u64>) -> Result<Found> {
    let sector = u64::from(src.sector_size());
    if sector == 0 {
        return Err(Error::Pit("the device reports a zero sector size".into()));
    }
    let mut left = budget.unwrap_or(u64::MAX);
    let mut read = 0u64;

    for gap in gaps {
        let start = gap.start.div_ceil(sector) * sector;
        let end = gap.end / sector * sector;
        if end <= start {
            continue;
        }
        let mut at = if gap.reverse { end } else { start };

        while left >= sector && (if gap.reverse { at > start } else { at < end }) {
            let room = if gap.reverse { at - start } else { end - at };
            let len = room.min(MAX_READ_BYTES).min(left) / sector * sector;
            let from = if gap.reverse { at - len } else { at };

            let buf = src.read(from / sector, len / sector)?;
            read += len;
            left -= len;

            let hits = (0..buf.len() / sector as usize).map(|i| i * sector as usize);
            let ordered: Vec<usize> = if gap.reverse {
                hits.rev().collect()
            } else {
                hits.collect()
            };
            for i in ordered {
                if u32::from_le_bytes(buf[i..i + 4].try_into().expect("4 bytes")) != MAGIC {
                    continue;
                }
                let offset = from + i as u64;
                if let Ok(pit) = read_at(src, offset) {
                    return Ok(Found { pit, offset });
                }
            }

            at = if gap.reverse { from } else { at + len };
        }
    }

    let looked = gaps
        .iter()
        .map(|gap| format!("{}..{}", gap.start, gap.end))
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::Pit(format!(
        "no PIT found in {read} bytes of the space no partition covers ({looked}); \
         pass --pit-offset if the table is elsewhere, or --pit-scan to search further"
    )))
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

    use crate::parts::{ImageSectors, Table, mbr::tests::write_table};

    const SECTOR: u64 = 512;
    /// The card in hand: one MBR partition, the table tucked into the space in
    /// front of it.
    const FIRST_PARTITION_LBA: u32 = 2048;

    /// An MBR-partitioned card with `pits` written at the given byte offsets.
    fn card_with_pits(pits: &[u64]) -> (ImageSectors, Vec<Gap>) {
        let size = 16 << 20;
        let mut data = vec![0u8; size];
        write_table(&mut data, 0, &[(0x0c, FIRST_PARTITION_LBA, 4096)]);
        let table = build(&[("BOOT", 2048, 128), ("LOG", 8192, 1024)]);
        for at in pits {
            let at = *at as usize;
            data[at..at + table.len()].copy_from_slice(&table);
        }
        let mut image = ImageSectors::new(data, SECTOR as u32);
        let parsed = crate::parts::read(&mut image, None).unwrap();
        let gaps = parsed.gaps(Some(size as u64));
        (image, gaps)
    }

    #[test]
    fn a_table_in_the_unallocated_space_is_found_by_its_magic() {
        let (mut image, gaps) = card_with_pits(&[1000 * SECTOR]);

        let found = scan(&mut image, &gaps, Some(DEFAULT_SCAN_BUDGET)).unwrap();

        assert_eq!(found.offset, 1000 * SECTOR);
        assert_eq!(found.pit.partitions.len(), 2);
    }

    /// The gap in front of the first partition is searched backwards, so the
    /// copy the partition was written against is the one that answers.
    #[test]
    fn the_copy_nearest_the_first_partition_answers_first() {
        let (mut image, gaps) = card_with_pits(&[100 * SECTOR, 2000 * SECTOR]);

        let found = scan(&mut image, &gaps, Some(DEFAULT_SCAN_BUDGET)).unwrap();

        assert_eq!(found.offset, 2000 * SECTOR);
    }

    /// Four bytes come up by chance; only a parse settles it.
    #[test]
    fn a_stray_magic_is_passed_over() {
        let (mut image, gaps) = card_with_pits(&[1000 * SECTOR]);
        let stray = 1500 * SECTOR as usize;
        image.data_mut()[stray..stray + 4].copy_from_slice(&MAGIC.to_le_bytes());
        image.data_mut()[stray + 4..stray + 8].copy_from_slice(&u32::MAX.to_le_bytes());

        let found = scan(&mut image, &gaps, Some(DEFAULT_SCAN_BUDGET)).unwrap();

        assert_eq!(found.offset, 1000 * SECTOR);
    }

    /// A full pass over a card is bounded by read throughput, so the search has
    /// to stop somewhere and say how to carry on.
    #[test]
    fn the_budget_bounds_the_search_and_the_message_says_what_to_do() {
        let (mut image, gaps) = card_with_pits(&[8 << 20]);

        let err = scan(&mut image, &gaps, Some(4096)).unwrap_err().to_string();

        assert!(err.contains("--pit-offset"), "{err}");
        assert!(err.contains("--pit-scan"), "{err}");
    }

    #[test]
    fn an_unlimited_budget_reaches_the_tail() {
        let (mut image, gaps) = card_with_pits(&[8 << 20]);

        assert_eq!(scan(&mut image, &gaps, None).unwrap().offset, 8 << 20);
    }

    /// Every read this tool makes is sector aligned; an offset that is not is a
    /// wrong argument, not a read to attempt.
    #[test]
    fn read_at_refuses_an_unaligned_offset() {
        let (mut image, _) = card_with_pits(&[1000 * SECTOR]);

        let err = read_at(&mut image, 1000 * SECTOR + 1)
            .unwrap_err()
            .to_string();

        assert!(err.contains("sector size"), "{err}");
    }

    #[test]
    fn read_at_reads_a_table_longer_than_one_sector() {
        let (mut image, _) = card_with_pits(&[1000 * SECTOR]);

        let pit = read_at(&mut image, 1000 * SECTOR).unwrap();

        assert_eq!(pit.partitions.len(), 2);
        assert!(HEADER_LEN + 2 * ENTRY_LEN > SECTOR as usize / 2);
    }

    /// A search over a device with no table at all must not report one.
    #[test]
    fn an_empty_card_finds_nothing() {
        let mut image = ImageSectors::new(vec![0u8; 1 << 20], SECTOR as u32);
        let gaps = Table {
            scheme: crate::parts::Scheme::Mbr,
            source: "test".into(),
            partitions: Vec::new(),
        }
        .gaps(Some(1 << 20));

        assert!(scan(&mut image, &gaps, None).is_err());
    }

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

    /// Garbage where the table was expected must fail on the magic, never
    /// reach the entry count: the count sizes an allocation and a device read.
    #[test]
    fn table_len_rejects_garbage_before_sizing_anything() {
        let message = Pit::table_len(&[0xFF; HEADER_LEN]).unwrap_err().to_string();
        assert!(message.contains("magic"), "{message}");

        assert!(Pit::table_len(&[0u8; HEADER_LEN - 1]).is_err());
    }

    #[test]
    fn table_len_caps_an_implausible_entry_count() {
        let mut head = build(&[]);
        head[4..8].copy_from_slice(&u32::MAX.to_le_bytes());

        let message = Pit::table_len(&head).unwrap_err().to_string();

        assert!(message.contains("implausible"), "{message}");
    }

    #[test]
    fn table_len_sizes_a_valid_header() {
        let bytes = build(&[("BOOT", 0, 1), ("LOG", 8, 2)]);
        assert_eq!(
            Pit::table_len(&bytes[..HEADER_LEN]).unwrap(),
            HEADER_LEN + 2 * ENTRY_LEN
        );
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
