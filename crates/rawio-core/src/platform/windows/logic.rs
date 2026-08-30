//! Windows-specific logic with no syscalls in it, so the whole test suite runs
//! on the development host.

use crate::device::Removability;

/// Win32 error codes that show up on the physical-disk path often enough to be
/// worth naming in the output. The numeric code is always printed too.
pub const ERROR_ACCESS_DENIED: i32 = 5;
pub const ERROR_SHARING_VIOLATION: i32 = 32;
pub const ERROR_WRITE_PROTECT: i32 = 19;
pub const ERROR_INVALID_PARAMETER: i32 = 87;
pub const ERROR_DEVICE_NOT_CONNECTED: i32 = 1167;
pub const ERROR_DEVICE_REMOVED: i32 = 1617;

/// `STORAGE_BUS_TYPE` values kept for classification.
pub const BUS_TYPE_USB: u32 = 0x07;
pub const BUS_TYPE_SD: u32 = 0x0C;
pub const BUS_TYPE_MMC: u32 = 0x0D;

/// Control codes, spelled out rather than pulled from a binding so the values
/// are visible next to the buffer layouts they go with.
pub const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002D_1400;
pub const IOCTL_DISK_GET_LENGTH_INFO: u32 = 0x0007_405C;
pub const IOCTL_DISK_GET_DRIVE_GEOMETRY: u32 = 0x0007_0000;
pub const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
pub const FSCTL_LOCK_VOLUME: u32 = 0x0009_0018;
pub const FSCTL_DISMOUNT_VOLUME: u32 = 0x0009_0020;

/// `STORAGE_PROPERTY_QUERY { StorageDeviceProperty, PropertyStandardQuery }`.
pub const STORAGE_DEVICE_PROPERTY_QUERY: [u8; 12] = [0; 12];

/// What `IOCTL_STORAGE_QUERY_PROPERTY` reports about a disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceDescriptor {
    pub removable_media: bool,
    pub bus_type: u32,
    pub vendor: String,
    pub product: String,
}

/// `STORAGE_DEVICE_DESCRIPTOR`: removable flag at 10, id offsets at 12 and 16,
/// bus type at 28. The id offsets are relative to the start of the buffer and
/// are zero when the field is absent.
pub fn parse_device_descriptor(buf: &[u8]) -> Option<DeviceDescriptor> {
    const HEADER: usize = 36;
    if buf.len() < HEADER {
        return None;
    }
    Some(DeviceDescriptor {
        removable_media: buf[10] != 0,
        bus_type: u32_at(buf, 28)?,
        vendor: ascii_at(buf, u32_at(buf, 12)? as usize),
        product: ascii_at(buf, u32_at(buf, 16)? as usize),
    })
}

/// `VOLUME_DISK_EXTENTS`: extent count at 0, then 24B extents from offset 8,
/// each starting with the physical disk number.
pub fn parse_disk_extents(buf: &[u8]) -> Vec<u32> {
    const EXTENT: usize = 24;
    const FIRST: usize = 8;

    let Some(count) = u32_at(buf, 0) else {
        return Vec::new();
    };
    (0..count as usize)
        .map_while(|i| {
            let at = FIRST + i * EXTENT;
            // A partial extent at the end is not a disk number, it is a short read.
            (buf.len() >= at + EXTENT)
                .then(|| u32_at(buf, at))
                .flatten()
        })
        .collect()
}

/// `DISK_GEOMETRY`: `BytesPerSector` is the last of six fields, at offset 20.
pub fn parse_bytes_per_sector(buf: &[u8]) -> Option<u32> {
    if buf.len() < 24 {
        return None;
    }
    u32_at(buf, 20).filter(|size| *size != 0)
}

/// `GET_LENGTH_INFO`: a single little-endian byte length.
pub fn parse_length_info(buf: &[u8]) -> Option<u64> {
    let bytes = buf.get(..8)?;
    Some(u64::from_le_bytes(bytes.try_into().expect("eight bytes")))
}

/// `DISK_GEOMETRY` as a size: cylinders (8B at 0) x `TracksPerCylinder` (at 12)
/// x `SectorsPerTrack` (at 16) x `BytesPerSector` (at 20). Rounded down to a
/// cylinder boundary, so it understates the disk; it is the fallback for
/// handles that may not carry the read access `GET_LENGTH_INFO` demands.
pub fn parse_geometry_size(buf: &[u8]) -> Option<u64> {
    let cylinders = u64::from_le_bytes(buf.get(..8)?.try_into().expect("eight bytes"));
    let size = cylinders
        .checked_mul(u64::from(u32_at(buf, 12)?))?
        .checked_mul(u64::from(u32_at(buf, 16)?))?
        .checked_mul(u64::from(u32_at(buf, 20)?))?;
    (size != 0).then_some(size)
}

/// Volumes are reached by drive letter. A volume with no letter is not mounted
/// by a filesystem either, so it cannot be the one blocking a write.
pub fn volume_path(letter: char) -> String {
    format!(r"\\.\{}:", letter.to_ascii_uppercase())
}

fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    let bytes = buf.get(at..at + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
}

/// NUL terminated ASCII at a buffer relative offset. Offset zero means the
/// field is absent.
fn ascii_at(buf: &[u8], at: usize) -> String {
    if at == 0 || at >= buf.len() {
        return String::new();
    }
    let rest = &buf[at..];
    let end = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    String::from_utf8_lossy(&rest[..end]).trim().to_string()
}

pub fn physical_drive_path(index: u32) -> String {
    format!(r"\\.\PhysicalDrive{index}")
}

/// Accepts the forms a user is likely to paste: the bare index, the drive name,
/// and the full device path produced by `rawio list`.
pub fn parse_device_id(id: &str) -> Result<u32, String> {
    let trimmed = id.trim();
    let tail = trimmed
        .strip_prefix(r"\\.\")
        .or_else(|| trimmed.strip_prefix(r"\\?\"))
        .unwrap_or(trimmed);
    let digits = strip_prefix_ignore_case(tail, "physicaldrive").unwrap_or(tail);
    digits
        .parse::<u32>()
        .map_err(|_| format!("{id:?} is not a physical drive (expected e.g. 2 or PhysicalDrive2)"))
}

fn strip_prefix_ignore_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    value
        .get(..prefix.len())
        .filter(|head| head.eq_ignore_ascii_case(prefix))
        .map(|_| &value[prefix.len()..])
}

/// `RemovableMedia` from `STORAGE_DEVICE_DESCRIPTOR` is the only signal
/// available: a USB external SSD reports the same as a USB SD card reader.
pub fn classify(removable_media: bool, bus_type: u32) -> Removability {
    match (removable_media, bus_type) {
        (true, _) => Removability::Removable,
        (false, BUS_TYPE_SD | BUS_TYPE_MMC) => Removability::Removable,
        (false, _) => Removability::Fixed,
    }
}

pub fn describe_error(code: i32) -> &'static str {
    match code {
        ERROR_ACCESS_DENIED => "access denied - run elevated",
        ERROR_SHARING_VIOLATION => "sharing violation - a volume on this disk is in use",
        ERROR_WRITE_PROTECT => "media is write protected",
        ERROR_INVALID_PARAMETER => "invalid parameter - offset or length is likely unaligned",
        ERROR_DEVICE_NOT_CONNECTED | ERROR_DEVICE_REMOVED => "device is gone",
        _ => "unexpected Win32 error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_path_uses_the_physical_drive_namespace() {
        assert_eq!(physical_drive_path(2), r"\\.\PhysicalDrive2");
    }

    #[test]
    fn device_id_accepts_index_name_and_full_path() {
        for id in [
            "2",
            "PhysicalDrive2",
            "physicaldrive2",
            r"\\.\PhysicalDrive2",
        ] {
            assert_eq!(parse_device_id(id).unwrap(), 2, "{id}");
        }
    }

    #[test]
    fn device_id_rejects_a_drive_letter() {
        assert!(parse_device_id("E:").is_err());
    }

    #[test]
    fn sd_and_mmc_buses_count_as_removable() {
        assert_eq!(classify(true, BUS_TYPE_USB), Removability::Removable);
        assert_eq!(classify(false, BUS_TYPE_SD), Removability::Removable);
        assert_eq!(classify(false, BUS_TYPE_MMC), Removability::Removable);
    }

    #[test]
    fn a_fixed_non_card_disk_is_never_writable() {
        assert_eq!(classify(false, 0x03), Removability::Fixed);
        assert!(!classify(false, 0x03).writable());
    }

    fn descriptor(removable: bool, bus: u32, vendor: &str, product: &str) -> Vec<u8> {
        let mut buf = vec![0u8; 36 + vendor.len() + product.len() + 2];
        buf[10] = u8::from(removable);
        buf[12..16].copy_from_slice(&36u32.to_le_bytes());
        buf[16..20].copy_from_slice(&(36 + vendor.len() as u32 + 1).to_le_bytes());
        buf[28..32].copy_from_slice(&bus.to_le_bytes());
        buf[36..36 + vendor.len()].copy_from_slice(vendor.as_bytes());
        let at = 36 + vendor.len() + 1;
        buf[at..at + product.len()].copy_from_slice(product.as_bytes());
        let size = buf.len() as u32;
        buf[4..8].copy_from_slice(&size.to_le_bytes());
        buf
    }

    #[test]
    fn a_descriptor_yields_the_removable_flag_bus_and_names() {
        let parsed =
            parse_device_descriptor(&descriptor(true, BUS_TYPE_SD, "Generic ", "SD/MMC")).unwrap();

        assert!(parsed.removable_media);
        assert_eq!(parsed.bus_type, BUS_TYPE_SD);
        assert_eq!(parsed.vendor, "Generic");
        assert_eq!(parsed.product, "SD/MMC");
    }

    #[test]
    fn a_descriptor_with_no_id_offsets_still_parses() {
        let mut buf = vec![0u8; 36];
        buf[4..8].copy_from_slice(&36u32.to_le_bytes());
        buf[28..32].copy_from_slice(&BUS_TYPE_USB.to_le_bytes());
        let parsed = parse_device_descriptor(&buf).unwrap();

        assert!(!parsed.removable_media);
        assert_eq!(parsed.vendor, "");
        assert_eq!(parsed.product, "");
    }

    #[test]
    fn a_truncated_descriptor_is_rejected() {
        assert_eq!(parse_device_descriptor(&[0u8; 20]), None);
    }

    #[test]
    fn disk_extents_list_every_physical_disk_a_volume_spans() {
        let mut buf = vec![0u8; 8 + 24 * 2];
        buf[0..4].copy_from_slice(&2u32.to_le_bytes());
        buf[8..12].copy_from_slice(&2u32.to_le_bytes());
        buf[32..36].copy_from_slice(&5u32.to_le_bytes());

        assert_eq!(parse_disk_extents(&buf), vec![2, 5]);
    }

    #[test]
    fn a_short_extent_buffer_yields_nothing() {
        assert!(parse_disk_extents(&[0u8; 4]).is_empty());
        let mut truncated = vec![0u8; 8 + 12];
        truncated[0..4].copy_from_slice(&1u32.to_le_bytes());
        assert!(parse_disk_extents(&truncated).is_empty());
    }

    #[test]
    fn geometry_reports_the_logical_sector_size() {
        let mut buf = vec![0u8; 24];
        buf[20..24].copy_from_slice(&4096u32.to_le_bytes());

        assert_eq!(parse_bytes_per_sector(&buf), Some(4096));
        assert_eq!(parse_bytes_per_sector(&[0u8; 24]), None);
        assert_eq!(parse_bytes_per_sector(&[0u8; 8]), None);
    }

    #[test]
    fn length_info_is_a_single_byte_count() {
        let mut buf = vec![0u8; 8];
        buf.copy_from_slice(&(31_914_983_424u64).to_le_bytes());

        assert_eq!(parse_length_info(&buf), Some(31_914_983_424));
        assert_eq!(parse_length_info(&[0u8; 4]), None);
    }

    #[test]
    fn geometry_multiplies_out_to_a_fallback_size() {
        let mut buf = vec![0u8; 24];
        buf[0..8].copy_from_slice(&3800u64.to_le_bytes()); // cylinders
        buf[12..16].copy_from_slice(&255u32.to_le_bytes()); // tracks per cylinder
        buf[16..20].copy_from_slice(&63u32.to_le_bytes()); // sectors per track
        buf[20..24].copy_from_slice(&512u32.to_le_bytes()); // bytes per sector

        assert_eq!(parse_geometry_size(&buf), Some(3800 * 255 * 63 * 512));
        assert_eq!(parse_geometry_size(&[0u8; 24]), None);
        assert_eq!(parse_geometry_size(&[0u8; 8]), None);
    }

    #[test]
    fn volumes_are_addressed_by_drive_letter() {
        assert_eq!(volume_path('E'), r"\\.\E:");
    }

    #[test]
    fn unaligned_access_maps_to_a_readable_hint() {
        assert!(describe_error(ERROR_INVALID_PARAMETER).contains("unaligned"));
    }
}
