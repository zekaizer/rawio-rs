//! GPT: a header at LBA 1 and an entry array it points at, both covered by
//! CRC32. The backup header sits at the last LBA and points at its own copy of
//! the array.
//!
//! Header, from offset 0: signature "EFI PART" (8), revision (4), header size
//! (4), header CRC32 (4), reserved (4), this LBA (8), alternate LBA (8), first
//! usable LBA (8), last usable LBA (8), disk GUID (16), entry array LBA (8),
//! entry count (4), entry size (4), entry array CRC32 (4).
//!
//! Entry, from offset 0: type GUID (16), unique GUID (16), first LBA (8), last
//! LBA (8, inclusive), attributes (8), name (72, UTF-16LE, NUL padded).
//!
//! Unlike the PIT, this layout is specified, so a CRC mismatch is a corrupt
//! table rather than a misread one, and it is reported as such.

use super::crc32::crc32;
use super::{MAX_READ_BYTES, Partition, Scheme, Sectors, Table};
use crate::error::{Error, Result};

pub const SIGNATURE: &[u8; 8] = b"EFI PART";
pub const PRIMARY_LBA: u64 = 1;
pub const MIN_HEADER_LEN: u32 = 92;
pub const MIN_ENTRY_LEN: u32 = 128;

const CRC_FIELD: std::ops::Range<usize> = 16..20;

/// Cheap probe used by detection: says whether LBA 1 claims to be a GPT,
/// without deciding whether the table behind it parses.
pub fn signature_present(src: &mut dyn Sectors) -> Result<bool> {
    let sector = src.read(PRIMARY_LBA, 1)?;
    Ok(sector.starts_with(SIGNATURE))
}

/// Reads the primary table, falling back to the backup. Which copy the entries
/// came from lands in `Table::source`, because a silent fallback would hide a
/// table that is already half gone.
pub fn parse(src: &mut dyn Sectors) -> Result<Table> {
    let primary = read_header(src, PRIMARY_LBA);
    let backup_lba = match &primary {
        Ok(header) => Some(header.alternate_lba),
        Err(_) => last_lba(src),
    };

    let primary_err = match primary {
        Ok(header) => match read_entries(src, &header) {
            Ok(partitions) => {
                return Ok(Table {
                    scheme: Scheme::Gpt,
                    source: format!("primary GPT header at LBA {PRIMARY_LBA}"),
                    partitions,
                });
            }
            Err(err) => err,
        },
        Err(err) => err,
    };

    let Some(backup_lba) = backup_lba.filter(|lba| *lba != PRIMARY_LBA) else {
        return Err(primary_err);
    };

    let header = read_header(src, backup_lba).map_err(|backup_err| {
        Error::Parts(format!(
            "the primary GPT is unusable ({primary_err}) and so is the backup at LBA \
             {backup_lba} ({backup_err})"
        ))
    })?;
    let partitions = read_entries(src, &header).map_err(|backup_err| {
        Error::Parts(format!(
            "the primary GPT is unusable ({primary_err}) and so is the backup at LBA \
             {backup_lba} ({backup_err})"
        ))
    })?;

    Ok(Table {
        scheme: Scheme::Gpt,
        source: format!("backup GPT header at LBA {backup_lba} (primary unusable: {primary_err})"),
        partitions,
    })
}

#[derive(Debug, Clone, Copy)]
struct Header {
    alternate_lba: u64,
    entry_lba: u64,
    entry_count: u32,
    entry_size: u32,
    entries_crc: u32,
}

/// Everything a header can be checked for is checked here, before any field of
/// it is allowed to size a read.
fn read_header(src: &mut dyn Sectors, lba: u64) -> Result<Header> {
    let sector = src.read(lba, 1)?;
    if !sector.starts_with(SIGNATURE) {
        return Err(Error::Parts(format!("no EFI PART signature at LBA {lba}")));
    }

    let header_size = u32_at(&sector, 12);
    let len = usize::try_from(header_size)
        .ok()
        .filter(|len| *len >= MIN_HEADER_LEN as usize && *len <= sector.len())
        .ok_or_else(|| {
            Error::Parts(format!(
                "header size {header_size} at LBA {lba} is not within {MIN_HEADER_LEN}..={}",
                sector.len()
            ))
        })?;

    let mut checked = sector[..len].to_vec();
    checked[CRC_FIELD].fill(0);
    let expected = u32_at(&sector, 16);
    let found = crc32(&checked);
    if found != expected {
        return Err(Error::Parts(format!(
            "header CRC32 at LBA {lba} is {found:#010x}, the header claims {expected:#010x}"
        )));
    }

    let my_lba = u64_at(&sector, 24);
    if my_lba != lba {
        return Err(Error::Parts(format!(
            "the header read at LBA {lba} says it lives at LBA {my_lba}"
        )));
    }

    let entry_size = u32_at(&sector, 84);
    if entry_size < MIN_ENTRY_LEN || entry_size % 8 != 0 {
        return Err(Error::Parts(format!(
            "entry size {entry_size} is not a multiple of 8 at or above {MIN_ENTRY_LEN}"
        )));
    }
    let entry_count = u32_at(&sector, 80);
    let array_bytes = u64::from(entry_count) * u64::from(entry_size);
    if entry_count == 0 || array_bytes > MAX_READ_BYTES {
        return Err(Error::Parts(format!(
            "{entry_count} entries of {entry_size} bytes is not a readable entry array"
        )));
    }

    Ok(Header {
        alternate_lba: u64_at(&sector, 32),
        entry_lba: u64_at(&sector, 72),
        entry_count,
        entry_size,
        entries_crc: u32_at(&sector, 88),
    })
}

fn read_entries(src: &mut dyn Sectors, header: &Header) -> Result<Vec<Partition>> {
    let sector = u64::from(src.sector_size());
    let bytes = u64::from(header.entry_count) * u64::from(header.entry_size);
    let sectors = bytes.div_ceil(sector);
    let array = src.read(header.entry_lba, sectors)?;

    let bytes = usize::try_from(bytes).expect("the header check caps the array");
    let found = crc32(&array[..bytes]);
    if found != header.entries_crc {
        return Err(Error::Parts(format!(
            "entry array CRC32 is {found:#010x}, the header claims {:#010x}",
            header.entries_crc
        )));
    }

    let size = header.entry_size as usize;
    let mut partitions = Vec::new();
    for index in 0..header.entry_count {
        let raw = &array[index as usize * size..][..size];
        if raw[..16].iter().all(|b| *b == 0) {
            continue;
        }
        let first = u64_at(raw, 32);
        let last = u64_at(raw, 40);
        if last < first {
            return Err(Error::Parts(format!(
                "entry {} ends at LBA {last}, before it starts at {first}",
                index + 1
            )));
        }
        let start = first
            .checked_mul(sector)
            .ok_or_else(|| Error::Parts(format!("entry {} starts past 2^64 bytes", index + 1)))?;
        let length = (last - first + 1).checked_mul(sector).ok_or_else(|| {
            Error::Parts(format!("entry {} is longer than 2^64 bytes", index + 1))
        })?;
        partitions.push(Partition {
            index: index + 1,
            name: Some(name_at(raw, 56)).filter(|name| !name.is_empty()),
            kind: guid_at(raw, 0),
            start,
            length,
        });
    }
    Ok(partitions)
}

/// Where the backup header sits when there is no primary header to point at it.
fn last_lba(src: &dyn Sectors) -> Option<u64> {
    let size = src.device_size()?;
    (size / u64::from(src.sector_size())).checked_sub(1)
}

/// Mixed-endian, as the spec defines it: the first three fields are little
/// endian, the last two are byte order as stored.
fn guid_at(raw: &[u8], at: usize) -> String {
    let g = &raw[at..at + 16];
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{}",
        u32::from_le_bytes(g[0..4].try_into().expect("4 bytes")),
        u16::from_le_bytes(g[4..6].try_into().expect("2 bytes")),
        u16::from_le_bytes(g[6..8].try_into().expect("2 bytes")),
        g[8],
        g[9],
        g[10..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
}

/// 72 bytes of UTF-16LE, NUL padded. Lone surrogates are replaced rather than
/// rejected: a name is printed, never acted on.
fn name_at(raw: &[u8], at: usize) -> String {
    let units = raw[at..at + 72]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes(pair.try_into().expect("2 bytes")))
        .take_while(|unit| *unit != 0);
    char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect::<String>()
        .trim()
        .to_string()
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("4 bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("8 bytes"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::ImageSectors;
    use super::super::mbr::tests::write_table;
    use super::*;

    pub(crate) const SECTOR: usize = 512;
    const ENTRY_LEN: usize = 128;
    const ENTRY_COUNT: u32 = 8;
    const ENTRY_LBA: u64 = 2;

    /// `(type GUID first byte, name, first LBA, last LBA)`.
    pub(crate) fn image_with_gpt(entries: &[(u8, &str, u64, u64)], sectors: usize) -> ImageSectors {
        let mut data = vec![0u8; sectors * SECTOR];
        // A protective MBR, so detection sees what a real GPT disk looks like.
        write_table(&mut data, 0, &[(0xEE, 1, (sectors - 1) as u32)]);
        let array = build_entries(entries);
        data[ENTRY_LBA as usize * SECTOR..][..array.len()].copy_from_slice(&array);
        let backup_lba = (sectors - 1) as u64;
        let header = build_header(PRIMARY_LBA, backup_lba, ENTRY_LBA, &array);
        data[SECTOR..][..header.len()].copy_from_slice(&header);
        ImageSectors::new(data, SECTOR as u32)
    }

    pub(crate) fn build_entries(entries: &[(u8, &str, u64, u64)]) -> Vec<u8> {
        let mut array = vec![0u8; ENTRY_COUNT as usize * ENTRY_LEN];
        for (i, (kind, name, first, last)) in entries.iter().enumerate() {
            let entry = &mut array[i * ENTRY_LEN..][..ENTRY_LEN];
            entry[0] = *kind;
            entry[3] = 0xAB;
            entry[16] = 0x11;
            entry[32..40].copy_from_slice(&first.to_le_bytes());
            entry[40..48].copy_from_slice(&last.to_le_bytes());
            for (j, unit) in name.encode_utf16().enumerate() {
                entry[56 + j * 2..58 + j * 2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        array
    }

    pub(crate) fn build_header(at: u64, alternate: u64, entry_lba: u64, array: &[u8]) -> Vec<u8> {
        let mut header = vec![0u8; SECTOR];
        header[0..8].copy_from_slice(SIGNATURE);
        header[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        header[12..16].copy_from_slice(&MIN_HEADER_LEN.to_le_bytes());
        header[24..32].copy_from_slice(&at.to_le_bytes());
        header[32..40].copy_from_slice(&alternate.to_le_bytes());
        header[72..80].copy_from_slice(&entry_lba.to_le_bytes());
        header[80..84].copy_from_slice(&ENTRY_COUNT.to_le_bytes());
        header[84..88].copy_from_slice(&(ENTRY_LEN as u32).to_le_bytes());
        header[88..92].copy_from_slice(&crc32(array).to_le_bytes());
        let crc = crc32(&header[..MIN_HEADER_LEN as usize]);
        header[16..20].copy_from_slice(&crc.to_le_bytes());
        header.truncate(SECTOR);
        header
    }

    #[test]
    fn resolves_entries_to_byte_ranges_and_names() {
        let mut image = image_with_gpt(
            &[(0x28, "boot", 2048, 4095), (0x0f, "rootfs", 4096, 8191)],
            16384,
        );

        let table = parse(&mut image).unwrap();

        assert_eq!(table.scheme, Scheme::Gpt);
        assert!(table.source.contains("primary"), "{}", table.source);
        assert_eq!(table.partitions.len(), 2);
        assert_eq!(table.partitions[0].name.as_deref(), Some("boot"));
        assert_eq!(table.partitions[0].start, 2048 * 512);
        // Last LBA is inclusive, so the length covers it.
        assert_eq!(table.partitions[0].length, 2048 * 512);
        assert_eq!(table.partitions[1].index, 2);
        assert_eq!(table.partitions[1].name.as_deref(), Some("rootfs"));
    }

    #[test]
    fn the_type_guid_is_printed_in_the_spelling_the_spec_uses() {
        let mut data = vec![0u8; 128];
        data[0..16].copy_from_slice(&[
            0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e,
            0xc9, 0x3b,
        ]);

        assert_eq!(guid_at(&data, 0), "c12a7328-f81f-11d2-ba4b-00a0c93ec93b");
    }

    #[test]
    fn a_corrupt_header_crc_is_refused() {
        let mut image = image_with_gpt(&[(0x28, "boot", 2048, 4095)], 16384);
        image.data_mut()[SECTOR + 40] ^= 0xFF;

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("CRC32"), "{err}");
    }

    #[test]
    fn a_corrupt_entry_array_is_refused() {
        let mut image = image_with_gpt(&[(0x28, "boot", 2048, 4095)], 16384);
        image.data_mut()[ENTRY_LBA as usize * SECTOR + 33] ^= 0xFF;

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("CRC32"), "{err}");
    }

    /// A card whose primary table was overwritten still has its layout at the
    /// far end, and using it must be visible in the output.
    #[test]
    fn a_broken_primary_falls_back_to_the_backup() {
        let sectors = 16384usize;
        let mut image = image_with_gpt(&[(0x28, "boot", 2048, 4095)], sectors);
        let backup_lba = (sectors - 1) as u64;
        let backup_entry_lba = backup_lba - 8;
        let array = build_entries(&[(0x28, "boot", 2048, 4095)]);
        let header = build_header(backup_lba, PRIMARY_LBA, backup_entry_lba, &array);
        let data = image.data_mut();
        data[backup_entry_lba as usize * SECTOR..][..array.len()].copy_from_slice(&array);
        data[backup_lba as usize * SECTOR..][..header.len()].copy_from_slice(&header);
        data[SECTOR..SECTOR + 8].fill(0);

        let table = parse(&mut image).unwrap();

        assert!(table.source.contains("backup"), "{}", table.source);
        assert!(
            table.source.contains("primary unusable"),
            "{}",
            table.source
        );
        assert_eq!(table.partitions.len(), 1);
    }

    #[test]
    fn an_implausible_entry_array_is_refused_before_it_sizes_a_read() {
        let mut image = image_with_gpt(&[(0x28, "boot", 2048, 4095)], 16384);
        let data = image.data_mut();
        data[SECTOR + 80..SECTOR + 84].copy_from_slice(&u32::MAX.to_le_bytes());
        let crc = crc32(&{
            let mut checked = data[SECTOR..SECTOR + MIN_HEADER_LEN as usize].to_vec();
            checked[CRC_FIELD].fill(0);
            checked
        });
        data[SECTOR + 16..SECTOR + 20].copy_from_slice(&crc.to_le_bytes());

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("readable entry array"), "{err}");
    }

    #[test]
    fn a_header_that_disowns_its_own_lba_is_refused() {
        let sectors = 16384usize;
        let mut image = image_with_gpt(&[(0x28, "boot", 2048, 4095)], sectors);
        let array = build_entries(&[(0x28, "boot", 2048, 4095)]);
        // A header claiming LBA 9, written at LBA 1: a copy left where it does
        // not belong, which is exactly what a stale image looks like.
        let header = build_header(9, (sectors - 1) as u64, ENTRY_LBA, &array);
        image.data_mut()[SECTOR..][..header.len()].copy_from_slice(&header);

        let err = parse(&mut image).unwrap_err();

        assert!(err.to_string().contains("says it lives at"), "{err}");
    }

    #[test]
    fn the_signature_probe_does_not_care_whether_the_table_parses() {
        let mut image = image_with_gpt(&[(0x28, "boot", 2048, 4095)], 16384);
        image.data_mut()[SECTOR + 16..SECTOR + 20].fill(0);

        assert!(signature_present(&mut image).unwrap());
        assert!(parse(&mut image).is_err());
    }
}
