//! Linux backend. Device identity and sysfs paths are pure logic; the ioctls are
//! `cfg`-gated.

use crate::device::{Access, Backend, DeviceInfo, RawDevice, Removability};
use crate::error::{DeviceError, Stage};

#[derive(Debug, Default)]
pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Backend for LinuxBackend {
    fn enumerate(&self) -> Result<Vec<DeviceInfo>, DeviceError> {
        Err(DeviceError::new(Stage::Enumerate, "not implemented yet"))
    }

    fn open(&self, id: &str, access: Access) -> Result<Box<dyn RawDevice>, DeviceError> {
        let name = device_name(id).map_err(|message| DeviceError::new(Stage::Open, message))?;
        let _ = (name, access);
        Err(DeviceError::new(Stage::Open, "not implemented yet"))
    }
}

/// Accepts `sda` and `/dev/sda`; rejects partitions, which are not raw devices.
pub fn device_name(id: &str) -> Result<String, String> {
    let name = id.trim().strip_prefix("/dev/").unwrap_or(id.trim());
    if name.is_empty() || name.contains('/') {
        return Err(format!("{id:?} is not a block device name"));
    }
    if name.starts_with("sd") && name.chars().last().is_some_and(|c| c.is_ascii_digit()) {
        return Err(format!("{id:?} is a partition; pass the whole device"));
    }
    if name.starts_with("mmcblk") && name.contains('p') {
        return Err(format!("{id:?} is a partition; pass the whole device"));
    }
    Ok(name.to_string())
}

pub fn sysfs_removable_path(name: &str) -> String {
    format!("/sys/block/{name}/removable")
}

/// `/sys/block/<dev>/removable` is 1 for removable media. Anything else,
/// including an unreadable attribute, stays unwritable.
pub fn classify(removable_attr: Option<&str>) -> Removability {
    match removable_attr.map(str::trim) {
        Some("1") => Removability::Removable,
        Some("0") => Removability::Fixed,
        _ => Removability::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_name_accepts_bare_and_dev_paths() {
        assert_eq!(device_name("sdb").unwrap(), "sdb");
        assert_eq!(device_name("/dev/mmcblk0").unwrap(), "mmcblk0");
    }

    #[test]
    fn partitions_are_rejected() {
        assert!(device_name("/dev/sdb1").is_err());
        assert!(device_name("mmcblk0p1").is_err());
    }

    #[test]
    fn sysfs_attribute_drives_the_removable_verdict() {
        assert_eq!(classify(Some("1\n")), Removability::Removable);
        assert_eq!(classify(Some("0\n")), Removability::Fixed);
        assert_eq!(classify(None), Removability::Unknown);
        assert!(!classify(None).writable());
    }

    #[test]
    fn sysfs_path_is_built_from_the_device_name() {
        assert_eq!(sysfs_removable_path("sdb"), "/sys/block/sdb/removable");
    }
}
