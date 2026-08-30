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

    #[test]
    fn unaligned_access_maps_to_a_readable_hint() {
        assert!(describe_error(ERROR_INVALID_PARAMETER).contains("unaligned"));
    }
}
