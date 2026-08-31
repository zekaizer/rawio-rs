//! MBR, including the EBR chain that logical partitions live on.
//!
//! Sector 0 holds four 16-byte entries at offset 446 and the two-byte `55 AA`
//! signature at 510. An entry is: status (1), CHS first (3), type (1), CHS last
//! (3), first LBA (4, LE), sector count (4, LE). CHS is ignored - every card
//! this tool touches is addressed by LBA.
//!
//! A type of 0x05, 0x0F or 0x85 is an extended container: its first sector is
//! an EBR whose first entry is a logical partition relative to that EBR, and
//! whose second entry, when present, points at the next EBR relative to the
//! container. Logical partitions are numbered from 5, as Linux numbers them.

use super::{Partition, Scheme, Sectors, Table};
use crate::error::{Error, Result};

pub const ENTRY_TABLE_AT: usize = 446;
pub const ENTRY_LEN: usize = 16;
pub const ENTRY_COUNT: usize = 4;
pub const SIGNATURE_AT: usize = 510;
pub const SIGNATURE: [u8; 2] = [0x55, 0xAA];

/// GPT's protective entry. Its presence means the real table is the GPT.
pub const TYPE_PROTECTIVE: u8 = 0xEE;

const TYPES_EXTENDED: [u8; 3] = [0x05, 0x0F, 0x85];

/// A chain longer than this is a loop or a corrupt table, either way not a
/// disk layout worth following further.
const MAX_LOGICAL: u32 = 128;

pub fn has_signature(sector: &[u8]) -> bool {
    sector.len() > SIGNATURE_AT + 1 && sector[SIGNATURE_AT..SIGNATURE_AT + 2] == SIGNATURE
}

/// True when any entry claims the protective type, which is how a GPT says the
/// MBR in front of it is not a table to read.
pub fn is_protective(sector: &[u8]) -> bool {
    (0..ENTRY_COUNT)
        .filter_map(|i| raw_entry(sector, i))
        .any(|entry| entry.kind == TYPE_PROTECTIVE)
}

pub fn parse(src: &mut dyn Sectors) -> Result<Table> {
    let sector0 = src.read(0, 1)?;
    if !has_signature(&sector0) {
        return Err(Error::Parts(
            "no 55AA signature at the end of LBA 0; this is not an MBR".into(),
        ));
    }

    let sector = u64::from(src.sector_size());
    let mut partitions = Vec::new();
    let mut extended = None;

    for index in 0..ENTRY_COUNT {
        let Some(entry) = raw_entry(&sector0, index) else {
            return Err(Error::Parts(format!("entry {index} is truncated")));
        };
        if entry.is_empty() {
            continue;
        }
        if TYPES_EXTENDED.contains(&entry.kind) {
            if extended.is_some() {
                return Err(Error::Parts(
                    "the MBR declares more than one extended partition".into(),
                ));
            }
            extended = Some(entry);
        }
        partitions.push(entry.to_partition(index as u32 + 1, sector)?);
    }

    if let Some(container) = extended {
        walk_chain(src, container, sector, &mut partitions)?;
    }

    Ok(Table {
        scheme: Scheme::Mbr,
        source: "MBR at LBA 0".to_string(),
        partitions,
    })
}

/// Follows the EBR chain. Every hop is bounded by the container, so a table
/// pointing outside it stops the walk instead of wandering the device.
fn walk_chain(
    src: &mut dyn Sectors,
    container: RawEntry,
    sector: u64,
    partitions: &mut Vec<Partition>,
) -> Result<()> {
    let base = u64::from(container.start_lba);
    let limit = base.saturating_add(u64::from(container.sectors));
    let mut next = Some(base);
    let mut index = 5;

    while let Some(ebr_lba) = next.take() {
        if index > 4 + MAX_LOGICAL {
            return Err(Error::Parts(format!(
                "the EBR chain is longer than {MAX_LOGICAL} entries; it is looping or corrupt"
            )));
        }
        if ebr_lba < base || ebr_lba >= limit {
            return Err(Error::Parts(format!(
                "an EBR at LBA {ebr_lba} is outside the extended partition {base}..{limit}"
            )));
        }

        let ebr = src.read(ebr_lba, 1)?;
        if !has_signature(&ebr) {
            return Err(Error::Parts(format!(
                "no 55AA signature on the EBR at LBA {ebr_lba}"
            )));
        }

        let logical = raw_entry(&ebr, 0).ok_or_else(|| {
            Error::Parts(format!("the EBR at LBA {ebr_lba} has a truncated entry"))
        })?;
        if logical.is_empty() {
            break;
        }
        let start = ebr_lba
            .checked_add(u64::from(logical.start_lba))
            .ok_or_else(|| {
                Error::Parts(format!("logical partition {index} overflows a byte offset"))
            })?;
        partitions.push(logical.to_partition_at(index, start, sector)?);
        index += 1;

        let link = raw_entry(&ebr, 1).ok_or_else(|| {
            Error::Parts(format!(
                "the EBR at LBA {ebr_lba} has a truncated link entry"
            ))
        })?;
        if !link.is_empty() {
            next = Some(base.saturating_add(u64::from(link.start_lba)));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct RawEntry {
    bootable: bool,
    kind: u8,
    start_lba: u32,
    sectors: u32,
}

impl RawEntry {
    fn is_empty(&self) -> bool {
        self.kind == 0 || self.sectors == 0
    }

    fn to_partition(self, index: u32, sector: u64) -> Result<Partition> {
        self.to_partition_at(index, u64::from(self.start_lba), sector)
    }

    fn to_partition_at(self, index: u32, start_lba: u64, sector: u64) -> Result<Partition> {
        let start = start_lba
            .checked_mul(sector)
            .ok_or_else(|| Error::Parts(format!("partition {index} starts past 2^64 bytes")))?;
        let length = u64::from(self.sectors)
            .checked_mul(sector)
            .ok_or_else(|| Error::Parts(format!("partition {index} is longer than 2^64 bytes")))?;
        Ok(Partition {
            index,
            name: None,
            kind: format!(
                "{:#04x}{}",
                self.kind,
                if self.bootable { " boot" } else { "" }
            ),
            start,
            length,
        })
    }
}

fn raw_entry(sector: &[u8], index: usize) -> Option<RawEntry> {
    let at = ENTRY_TABLE_AT + index * ENTRY_LEN;
    let raw = sector.get(at..at + ENTRY_LEN)?;
    Some(RawEntry {
        bootable: raw[0] == 0x80,
        kind: raw[4],
        start_lba: u32::from_le_bytes(raw[8..12].try_into().expect("4 bytes")),
        sectors: u32::from_le_bytes(raw[12..16].try_into().expect("4 bytes")),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::ImageSectors;
    use super::*;

    /// `(type, first LBA, sectors)` in the four primary slots, in order.
    pub(crate) fn image_with_entries(entries: &[(u8, u32, u32)], size: usize) -> ImageSectors {
        let mut data = vec![0u8; size];
        write_table(&mut data, 0, entries);
        ImageSectors::new(data, 512)
    }

    pub(crate) fn write_table(data: &mut [u8], at: usize, entries: &[(u8, u32, u32)]) {
        data[at + SIGNATURE_AT..at + SIGNATURE_AT + 2].copy_from_slice(&SIGNATURE);
        for (i, (kind, start, sectors)) in entries.iter().enumerate() {
            let entry = &mut data[at + ENTRY_TABLE_AT + i * ENTRY_LEN..][..ENTRY_LEN];
            entry[4] = *kind;
            entry[8..12].copy_from_slice(&start.to_le_bytes());
            entry[12..16].copy_from_slice(&sectors.to_le_bytes());
        }
    }

    #[test]
    fn resolves_primary_entries_to_byte_ranges() {
        let mut image =
            image_with_entries(&[(0x0c, 2048, 131072), (0x83, 133120, 262144)], 1 << 28);

        let table = parse(&mut image).unwrap();

        assert_eq!(table.scheme, Scheme::Mbr);
        assert_eq!(table.partitions.len(), 2);
        assert_eq!(table.partitions[0].index, 1);
        assert_eq!(table.partitions[0].kind, "0x0c");
        assert_eq!(table.partitions[0].start, 2048 * 512);
        assert_eq!(table.partitions[0].length, 131072 * 512);
        assert_eq!(table.partitions[1].index, 2);
        assert_eq!(table.partitions[1].start, 133120 * 512);
    }

    #[test]
    fn empty_slots_do_not_shift_the_indices_of_the_ones_that_follow() {
        let mut image = image_with_entries(&[(0, 0, 0), (0x83, 2048, 2048)], 1 << 24);

        let table = parse(&mut image).unwrap();

        assert_eq!(table.partitions.len(), 1);
        assert_eq!(table.partitions[0].index, 2);
    }

    #[test]
    fn a_4k_sector_size_scales_every_range() {
        let mut data = vec![0u8; 1 << 24];
        write_table(&mut data, 0, &[(0x83, 256, 512)]);
        let mut image = ImageSectors::new(data, 4096);

        let table = parse(&mut image).unwrap();

        assert_eq!(table.partitions[0].start, 256 * 4096);
        assert_eq!(table.partitions[0].length, 512 * 4096);
    }

    #[test]
    fn an_unsigned_sector_is_not_a_table() {
        let mut image = ImageSectors::new(vec![0u8; 1 << 20], 512);

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("not an MBR"), "{err}");
    }

    #[test]
    fn a_bootable_flag_is_visible_in_the_type_column() {
        let mut image = image_with_entries(&[(0x0c, 2048, 2048)], 1 << 24);
        image.data_mut()[ENTRY_TABLE_AT] = 0x80;

        let table = parse(&mut image).unwrap();

        assert_eq!(table.partitions[0].kind, "0x0c boot");
    }

    #[test]
    fn the_protective_entry_is_recognised() {
        let image = image_with_entries(&[(TYPE_PROTECTIVE, 1, 0xFFFF_FFFF)], 1 << 20);
        let mut image = image;
        let sector = image.read(0, 1).unwrap();

        assert!(is_protective(&sector));
        assert!(has_signature(&sector));
    }

    /// The chain is what makes logical partitions addressable at all.
    #[test]
    fn logical_partitions_are_numbered_from_five() {
        let mut data = vec![0u8; 1 << 26];
        write_table(&mut data, 0, &[(0x83, 2048, 2048), (0x05, 4096, 40960)]);
        // First EBR at LBA 4096: a logical at +2048, linking to the next at +8192.
        write_table(
            &mut data,
            4096 * 512,
            &[(0x83, 2048, 2048), (0x05, 8192, 8192)],
        );
        // Second EBR at LBA 4096+8192: one more logical, no further link.
        write_table(&mut data, (4096 + 8192) * 512, &[(0x83, 2048, 4096)]);
        let mut image = ImageSectors::new(data, 512);

        let table = parse(&mut image).unwrap();

        let indices: Vec<u32> = table.partitions.iter().map(|p| p.index).collect();
        assert_eq!(indices, vec![1, 2, 5, 6]);
        assert_eq!(table.partitions[2].start, (4096 + 2048) * 512);
        assert_eq!(table.partitions[3].start, (4096 + 8192 + 2048) * 512);
    }

    /// A chain that points back at itself must stop, not spin.
    #[test]
    fn a_looping_chain_is_refused() {
        let mut data = vec![0u8; 1 << 26];
        write_table(&mut data, 0, &[(0x05, 4096, 40960)]);
        write_table(
            &mut data,
            4096 * 512,
            &[(0x83, 2048, 2048), (0x05, 0, 8192)],
        );
        let mut image = ImageSectors::new(data, 512);

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("looping"), "{err}");
    }

    #[test]
    fn an_ebr_outside_its_container_stops_the_walk() {
        let mut data = vec![0u8; 1 << 26];
        write_table(&mut data, 0, &[(0x05, 4096, 4096)]);
        write_table(
            &mut data,
            4096 * 512,
            &[(0x83, 100, 100), (0x05, 60000, 8192)],
        );
        let mut image = ImageSectors::new(data, 512);

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("outside the extended"), "{err}");
    }

    #[test]
    fn two_extended_containers_are_refused() {
        let mut image = image_with_entries(&[(0x05, 4096, 4096), (0x0f, 20480, 4096)], 1 << 26);

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("more than one extended"), "{err}");
    }
}
