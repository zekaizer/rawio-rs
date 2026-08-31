//! Read-only interpretation of the partition tables an SD card really carries.
//!
//! MBR and GPT both sit at a fixed LBA and both carry a signature, so which of
//! them is present can be detected. The PIT cannot be - its location is an
//! argument, not a constant - so it stays in [`crate::pit`] and is never what
//! detection lands on.
//!
//! Nothing here writes, and every parse failure aborts before a range is used.

mod crc32;
pub mod gpt;
pub mod mbr;

use crate::device::RawDevice;
use crate::error::{Error, Result, Stage};
use crate::trace::Trace;

/// The two schemes that can be detected. `pit` is a scheme on the command line
/// but not here: it is chosen, never found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheme {
    Mbr,
    Gpt,
}

impl Scheme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Scheme::Mbr => "mbr",
            Scheme::Gpt => "gpt",
        }
    }
}

impl std::fmt::Display for Scheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One entry, in bytes. The scheme's own units are resolved during the parse so
/// nothing above this layer has to know a sector size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    /// 1-based. MBR numbers primaries 1-4 and logicals from 5, as Linux does.
    pub index: u32,
    /// GPT only - an MBR entry has no name field.
    pub name: Option<String>,
    /// Spelled as the table spells it: a hex byte for MBR, the type GUID for
    /// GPT. No name table, so nothing here can go out of date.
    pub kind: String,
    pub start: u64,
    pub length: u64,
}

impl Partition {
    /// One past the last byte, saturating: a table claiming the end of the
    /// address space is a table to report, not to panic on.
    pub fn end(&self) -> u64 {
        self.start.saturating_add(self.length)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub scheme: Scheme,
    /// Which copy of the table the entries came from, printed so a fallback to
    /// the backup GPT is never silent.
    pub source: String,
    pub partitions: Vec<Partition>,
}

impl Table {
    /// Only GPT entries have names, so a miss says which it is: an MBR lookup
    /// by name is a wrong selector, not a missing partition.
    pub fn find(&self, name: &str) -> Result<&Partition> {
        if self.scheme == Scheme::Mbr {
            return Err(Error::Parts(
                "MBR partitions have no names; select one with --partition-id N".into(),
            ));
        }
        self.partitions
            .iter()
            .find(|p| {
                p.name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| {
                let names = self
                    .partitions
                    .iter()
                    .map(|p| p.name.clone().unwrap_or_else(|| format!("#{}", p.index)))
                    .collect::<Vec<_>>()
                    .join(", ");
                Error::Parts(format!(
                    "no {} partition named {name:?}; the table has: {names}",
                    self.scheme
                ))
            })
    }

    pub fn find_by_index(&self, index: u32) -> Result<&Partition> {
        self.partitions
            .iter()
            .find(|p| p.index == index)
            .ok_or_else(|| {
                let have = self
                    .partitions
                    .iter()
                    .map(|p| p.index.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                Error::Parts(format!(
                    "no {} partition with index {index}; the table has: {have}",
                    self.scheme
                ))
            })
    }
}

/// A stretch of the device no partition covers. A table that is not itself a
/// partition has to be sitting in one of these.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    pub start: u64,
    pub end: u64,
    /// Searched from `end` downwards. A table tucked into the space in front of
    /// the first partition is tucked against that partition, not against LBA 0,
    /// so searching backwards finds it in the first block read.
    pub reverse: bool,
}

impl Gap {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Table {
    /// Everything the partitions leave over, in device order, with the space in
    /// front of the first partition marked to be searched backwards.
    ///
    /// The tail is only reported when the device size is known; without it
    /// there is nothing to bound the last gap with.
    pub fn gaps(&self, device_size: Option<u64>) -> Vec<Gap> {
        let mut ranges: Vec<(u64, u64)> = self
            .partitions
            .iter()
            .filter(|p| p.length > 0)
            .map(|p| (p.start, p.end()))
            .collect();
        ranges.sort_unstable();

        let mut occupied: Vec<(u64, u64)> = Vec::new();
        for (start, end) in ranges {
            match occupied.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => occupied.push((start, end)),
            }
        }

        let mut gaps = Vec::new();
        let mut at = 0u64;
        for (start, end) in &occupied {
            if *start > at {
                gaps.push(Gap {
                    start: at,
                    end: *start,
                    reverse: at == 0,
                });
            }
            at = at.max(*end);
        }
        if let Some(size) = device_size
            && size > at
        {
            gaps.push(Gap {
                start: at,
                end: size,
                reverse: occupied.is_empty(),
            });
        }
        gaps
    }
}

/// Sector-addressed reads. The parsers take this rather than a device so an
/// image in a test drives exactly the code a card does, and because whole
/// sectors are the only thing a physical-disk handle on Windows will serve.
pub trait Sectors {
    fn sector_size(&self) -> u32;
    /// Absent when the platform would not say, which is what makes the backup
    /// GPT unreachable without a primary header to point at it.
    fn device_size(&self) -> Option<u64>;
    fn read(&mut self, lba: u64, count: u64) -> Result<Vec<u8>>;
}

/// Reads no larger than this are ever asked for; a table claiming more is
/// rejected before it sizes an allocation or a device read.
pub const MAX_READ_BYTES: u64 = 4 << 20;

/// Sector reads against a real device, with each one on the trace.
pub struct DeviceSectors<'a> {
    device: &'a mut dyn RawDevice,
    trace: &'a Trace,
}

impl<'a> DeviceSectors<'a> {
    pub fn new(device: &'a mut dyn RawDevice, trace: &'a Trace) -> Self {
        Self { device, trace }
    }
}

impl Sectors for DeviceSectors<'_> {
    fn sector_size(&self) -> u32 {
        self.device.info().logical_sector_size
    }

    fn device_size(&self) -> Option<u64> {
        self.device.info().size_bytes
    }

    fn read(&mut self, lba: u64, count: u64) -> Result<Vec<u8>> {
        let (offset, len) = span(self.sector_size(), lba, count)?;
        let mut buf = vec![0u8; len];
        self.device.read_at(offset, &mut buf).map_err(|err| {
            self.trace
                .failed(format!("read {count} sector(s) at LBA {lba}"), &err);
            Error::Device(err)
        })?;
        self.trace.ok(
            Stage::ParseParts,
            format!("read {count} sector(s) at LBA {lba}"),
            "ok",
        );
        Ok(buf)
    }
}

/// An image in memory, for tests and for anything that already holds the bytes.
pub struct ImageSectors {
    data: Vec<u8>,
    sector_size: u32,
}

impl ImageSectors {
    pub fn new(data: Vec<u8>, sector_size: u32) -> Self {
        Self { data, sector_size }
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

impl Sectors for ImageSectors {
    fn sector_size(&self) -> u32 {
        self.sector_size
    }

    fn device_size(&self) -> Option<u64> {
        Some(self.data.len() as u64)
    }

    fn read(&mut self, lba: u64, count: u64) -> Result<Vec<u8>> {
        let (offset, len) = span(self.sector_size, lba, count)?;
        let start = usize::try_from(offset)
            .ok()
            .filter(|start| start.saturating_add(len) <= self.data.len())
            .ok_or_else(|| {
                Error::Parts(format!("LBA {lba}+{count} is past the end of the image"))
            })?;
        Ok(self.data[start..start + len].to_vec())
    }
}

/// Byte offset and length of a sector run, refusing anything that would size an
/// allocation from an unvalidated field.
fn span(sector_size: u32, lba: u64, count: u64) -> Result<(u64, usize)> {
    let sector = u64::from(sector_size);
    let len = count
        .checked_mul(sector)
        .filter(|len| *len > 0 && *len <= MAX_READ_BYTES)
        .ok_or_else(|| Error::Parts(format!("{count} sectors is not a readable run")))?;
    let offset = lba
        .checked_mul(sector)
        .ok_or_else(|| Error::Parts(format!("LBA {lba} overflows a byte offset")))?;
    Ok((offset, len as usize))
}

/// Reads the table, detecting the scheme when the caller did not fix one.
///
/// Detection only ever concludes what a signature proves. A layout carrying
/// both a real MBR and a GPT is ambiguous, and an ambiguous table resolving
/// silently to a plausible range is the failure that costs a card, so it is
/// refused rather than preferred one way.
pub fn read(src: &mut dyn Sectors, scheme: Option<Scheme>) -> Result<Table> {
    match scheme {
        Some(Scheme::Mbr) => mbr::parse(src),
        Some(Scheme::Gpt) => gpt::parse(src),
        // A range cannot come from a table that is not there, so an absence is
        // an error here even though it is a finding to whoever is only looking.
        None => match detect(src)? {
            Detected::Table(table) => Ok(table),
            Detected::None { reason } => Err(Error::Parts(reason)),
        },
    }
}

/// What [`detect`] concluded. A table that is ambiguous or damaged is an error
/// from `detect` itself; this only tells a table apart from the absence of one,
/// which is a thing a device is allowed to be.
pub enum Detected {
    Table(Table),
    /// Nothing here can read, and what the caller might try instead.
    None {
        reason: String,
    },
}

/// The table the device carries, if it carries one this can read. A hybrid
/// layout is ambiguous rather than absent, and comes back as an error.
pub fn detect(src: &mut dyn Sectors) -> Result<Detected> {
    let sector0 = src.read(0, 1)?;

    if !mbr::has_signature(&sector0) {
        return match gpt::parse(src) {
            Ok(table) => Ok(Detected::Table(table)),
            Err(err) => Ok(Detected::None {
                reason: format!(
                    "no MBR signature at LBA 0 and no usable GPT ({err}); \
                     pass --scheme pit if this device carries a PIT"
                ),
            }),
        };
    }

    if mbr::is_protective(&sector0) {
        return gpt::parse(src).map(Detected::Table);
    }

    let table = mbr::parse(src)?;
    let gpt_present = gpt::signature_present(src)?;
    match (table.partitions.is_empty(), gpt_present) {
        (false, true) => Err(Error::Parts(
            "LBA 0 holds a real MBR and LBA 1 holds a GPT signature; this hybrid layout \
             is ambiguous, so pass --scheme mbr or --scheme gpt"
                .into(),
        )),
        (false, false) => Ok(Detected::Table(table)),
        (true, true) => gpt::parse(src).map(Detected::Table),
        (true, false) => Ok(Detected::None {
            reason: "the MBR at LBA 0 has no partition entries and there is no GPT; \
                     pass --scheme mbr to see it anyway, or --scheme pit for a PIT"
                .into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::mbr::tests::image_with_entries;
    use super::*;

    #[test]
    fn a_run_past_the_read_cap_is_refused_before_it_allocates() {
        let mut image = ImageSectors::new(vec![0u8; 512], 512);

        let err = image.read(0, MAX_READ_BYTES).unwrap_err();

        assert!(err.to_string().contains("readable run"), "{err}");
    }

    #[test]
    fn detection_lands_on_mbr_when_only_an_mbr_is_there() {
        let mut image = image_with_entries(&[(0x0c, 2048, 4096)], 64 << 20);

        let table = read(&mut image, None).unwrap();

        assert_eq!(table.scheme, Scheme::Mbr);
        assert_eq!(table.partitions.len(), 1);
    }

    #[test]
    fn detection_refuses_a_hybrid_layout() {
        let mut image = image_with_entries(&[(0x0c, 2048, 4096)], 64 << 20);
        image.data_mut()[512..520].copy_from_slice(b"EFI PART");

        let err = read(&mut image, None).unwrap_err();

        assert!(err.to_string().contains("--scheme"), "{err}");
    }

    /// A PIT card has no signature at LBA 0, and the way out has to be in the
    /// message: there is no prompt to fall back on.
    #[test]
    fn detection_on_an_unsigned_device_points_at_the_pit() {
        let mut image = ImageSectors::new(vec![0u8; 1 << 20], 512);

        let err = read(&mut image, None).unwrap_err();

        assert!(err.to_string().contains("--scheme pit"), "{err}");
    }

    #[test]
    fn detection_on_an_empty_mbr_says_how_to_see_it_anyway() {
        let mut image = image_with_entries(&[], 1 << 20);

        let err = read(&mut image, None).unwrap_err();

        assert!(err.to_string().contains("--scheme mbr"), "{err}");
        assert_eq!(read(&mut image, Some(Scheme::Mbr)).unwrap().partitions, []);
    }

    /// An MBR lookup by name is the wrong selector, not a missing partition,
    /// and the message has to say which.
    #[test]
    fn the_gap_in_front_of_the_first_partition_is_searched_backwards() {
        let mut image = image_with_entries(&[(0x0c, 2048, 2048), (0x83, 8192, 2048)], 16 << 20);
        let table = read(&mut image, None).unwrap();

        let gaps = table.gaps(Some(16 << 20));

        assert_eq!(
            gaps,
            vec![
                Gap {
                    start: 0,
                    end: 2048 * 512,
                    reverse: true
                },
                Gap {
                    start: 4096 * 512,
                    end: 8192 * 512,
                    reverse: false
                },
                Gap {
                    start: 10240 * 512,
                    end: 16 << 20,
                    reverse: false
                },
            ]
        );
    }

    /// The scheme has nothing to do with it: a gap is whatever the entries
    /// leave over, and GPT entries leave over the same shape MBR ones do.
    #[test]
    fn gaps_are_the_same_idea_under_gpt() {
        let mut image = super::gpt::tests::image_with_gpt(&[(0x28, "boot", 2048, 4095)], 16384);
        let table = read(&mut image, None).unwrap();

        let gaps = table.gaps(Some(16384 * 512));

        assert_eq!(table.scheme, Scheme::Gpt);
        assert_eq!(
            gaps,
            vec![
                Gap {
                    start: 0,
                    end: 2048 * 512,
                    reverse: true
                },
                Gap {
                    start: 4096 * 512,
                    end: 16384 * 512,
                    reverse: false
                },
            ]
        );
    }

    /// Overlapping entries are a corrupt table, but the search still has to be
    /// given a coherent set of holes to look in.
    #[test]
    fn overlapping_entries_merge_into_one_occupied_run() {
        let table = Table {
            scheme: Scheme::Mbr,
            source: "test".into(),
            partitions: vec![
                Partition {
                    index: 1,
                    name: None,
                    kind: "0x83".into(),
                    start: 1000,
                    length: 500,
                },
                Partition {
                    index: 2,
                    name: None,
                    kind: "0x83".into(),
                    start: 1200,
                    length: 800,
                },
            ],
        };

        assert_eq!(
            table.gaps(Some(4000)),
            vec![
                Gap {
                    start: 0,
                    end: 1000,
                    reverse: true
                },
                Gap {
                    start: 2000,
                    end: 4000,
                    reverse: false
                },
            ]
        );
    }

    #[test]
    fn an_mbr_name_lookup_names_the_selector_that_works() {
        let table = Table {
            scheme: Scheme::Mbr,
            source: "test".into(),
            partitions: Vec::new(),
        };

        let err = table.find("boot").unwrap_err();

        assert!(err.to_string().contains("--partition-id"), "{err}");
    }

    #[test]
    fn an_unknown_index_reports_the_indices_that_exist() {
        let mut image = image_with_entries(&[(0x0c, 2048, 4096), (0x83, 8192, 4096)], 64 << 20);
        let table = read(&mut image, None).unwrap();

        let err = table.find_by_index(3).unwrap_err();

        assert!(err.to_string().contains("1, 2"), "{err}");
    }
}
