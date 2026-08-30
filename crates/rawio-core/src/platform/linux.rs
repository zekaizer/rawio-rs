//! Linux backend. Geometry and removability come from sysfs rather than ioctls,
//! so the only thing that needs a real Linux host is opening `/dev/<name>`.

use crate::device::{Access, Backend, DeviceInfo, RawDevice, Removability, VolumeLock};
use crate::error::{DeviceError, Stage};
use crate::trace::Trace;

const SYSFS_BLOCK: &str = "/sys/block";

/// `/sys/block/<name>/size` is counted in 512B units on every device, whatever
/// its logical block size is.
const SIZE_UNIT: u64 = 512;

#[derive(Debug, Default)]
pub struct LinuxBackend;

impl LinuxBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Backend for LinuxBackend {
    fn enumerate(&self, trace: &Trace) -> Result<Vec<DeviceInfo>, DeviceError> {
        trace.ok(Stage::Enumerate, SYSFS_BLOCK, "scanning");
        let entries = std::fs::read_dir(SYSFS_BLOCK)
            .map_err(|err| DeviceError::from_io(Stage::Enumerate, &err))?;

        let mut devices = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| DeviceError::from_io(Stage::Enumerate, &err))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if is_candidate(&name) {
                devices.push(device_info(&name, &read_attrs(&name)));
            }
        }
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(devices)
    }

    fn open(
        &self,
        id: &str,
        access: Access,
        trace: &Trace,
    ) -> Result<Box<dyn RawDevice>, DeviceError> {
        let name = device_name(id).map_err(|message| DeviceError::new(Stage::Open, message))?;
        require_whole_device(&name).inspect_err(|err| {
            trace.failed(format!("/dev/{name}"), err);
        })?;
        let device = open_device(&name, access).inspect_err(|err| {
            trace.failed(format!("/dev/{name}"), err);
        })?;
        trace.ok(Stage::Open, format!("/dev/{name}"), "handle acquired");
        Ok(device)
    }

    /// Linux has no volume lock to take: a raw write to a device with a mounted
    /// filesystem is permitted. Taking a writable handle is the whole check.
    fn rehearse_write(&self, id: &str, trace: &Trace) -> Result<Vec<VolumeLock>, DeviceError> {
        let name = device_name(id).map_err(|message| DeviceError::new(Stage::Open, message))?;
        require_whole_device(&name).inspect_err(|err| {
            trace.failed(format!("/dev/{name}"), err);
        })?;
        let device = open_device(&name, Access::ReadWrite).inspect_err(|err| {
            trace.failed(format!("/dev/{name}"), err);
        })?;
        drop(device);
        trace.ok(
            Stage::Open,
            format!("/dev/{name}"),
            "read-write, for rehearsal only",
        );
        Ok(Vec::new())
    }
}

#[cfg(unix)]
fn open_device(name: &str, access: Access) -> Result<Box<dyn RawDevice>, DeviceError> {
    let info = device_info(name, &read_attrs(name));
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(matches!(access, Access::ReadWrite))
        .open(&info.id)
        .map_err(|err| DeviceError::from_io(Stage::Open, &err))?;
    Ok(Box::new(LinuxDevice { info, file }))
}

#[cfg(not(unix))]
fn open_device(name: &str, access: Access) -> Result<Box<dyn RawDevice>, DeviceError> {
    let _ = (name, access);
    Err(DeviceError::new(
        Stage::Open,
        "the Linux backend requires a Unix host",
    ))
}

#[cfg(unix)]
struct LinuxDevice {
    info: DeviceInfo,
    file: std::fs::File,
}

#[cfg(unix)]
impl RawDevice for LinuxDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        use std::os::unix::fs::FileExt;
        self.file
            .read_exact_at(buf, offset)
            .map_err(|err| DeviceError::from_io(Stage::Read, &err))?;
        Ok(buf.len())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, DeviceError> {
        use std::os::unix::fs::FileExt;
        self.file
            .write_all_at(buf, offset)
            .map_err(|err| DeviceError::from_io(Stage::Write, &err))?;
        Ok(buf.len())
    }

    /// Buffered writes are only on the medium after this returns.
    fn flush(&mut self) -> Result<(), DeviceError> {
        self.file
            .sync_all()
            .map_err(|err| DeviceError::from_io(Stage::Flush, &err))
    }
}

fn read_attrs(name: &str) -> SysfsAttrs {
    let dir = format!("{SYSFS_BLOCK}/{name}");
    SysfsAttrs {
        size: read_attr(&format!("{dir}/size")),
        logical_block_size: read_attr(&format!("{dir}/queue/logical_block_size")),
        removable: read_attr(&format!("{dir}/removable")),
        vendor: read_attr(&format!("{dir}/device/vendor")),
        model: read_attr(&format!("{dir}/device/model")),
    }
}

fn read_attr(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Accepts `sda` and `/dev/sda`; rejects partitions, which are not raw devices.
///
/// The patterns cover the common naming schemes only; `require_whole_device`
/// is the general guard, and needs the live sysfs the tests do not have.
pub fn device_name(id: &str) -> Result<String, String> {
    let name = id.trim().strip_prefix("/dev/").unwrap_or(id.trim());
    if name.is_empty() || name.contains('/') {
        return Err(format!("{id:?} is not a block device name"));
    }
    // Disks named with a trailing letter take bare partition numbers...
    let lettered = ["sd", "hd", "vd", "xvd"];
    if lettered.iter().any(|prefix| name.starts_with(prefix))
        && name.chars().last().is_some_and(|c| c.is_ascii_digit())
    {
        return Err(format!("{id:?} is a partition; pass the whole device"));
    }
    // ...disks named with a trailing digit get a `p` separator instead.
    let numbered = ["mmcblk", "nvme"];
    if numbered.iter().any(|prefix| name.starts_with(prefix)) && name.contains('p') {
        return Err(format!("{id:?} is a partition; pass the whole device"));
    }
    Ok(name.to_string())
}

/// Partitions appear under their disk in sysfs, never at the top level, so
/// `/sys/block` membership rejects every partition scheme the name patterns
/// do not know - and typos with it.
fn require_whole_device(name: &str) -> Result<(), DeviceError> {
    if std::path::Path::new(SYSFS_BLOCK).join(name).exists() {
        return Ok(());
    }
    Err(DeviceError::new(
        Stage::Open,
        format!("/dev/{name} is not a whole block device: {SYSFS_BLOCK}/{name} does not exist"),
    ))
}

pub fn sysfs_removable_path(name: &str) -> String {
    format!("/sys/block/{name}/removable")
}

/// Raw `/sys/block/<name>/` attribute values. `None` is an absent or unreadable
/// attribute, which several of these are on virtual devices.
#[derive(Debug, Default, Clone)]
pub struct SysfsAttrs {
    /// `size`, always counted in 512B units whatever the logical block size is.
    pub size: Option<String>,
    /// `queue/logical_block_size`.
    pub logical_block_size: Option<String>,
    /// `removable`.
    pub removable: Option<String>,
    /// `device/vendor`.
    pub vendor: Option<String>,
    /// `device/model`.
    pub model: Option<String>,
}

/// Virtual and pseudo devices that can never be a target.
pub fn is_candidate(name: &str) -> bool {
    const VIRTUAL: [&str; 8] = ["loop", "ram", "zram", "dm-", "md", "sr", "nbd", "fd"];
    !name.is_empty() && !VIRTUAL.iter().any(|prefix| name.starts_with(prefix))
}

pub fn device_info(name: &str, attrs: &SysfsAttrs) -> DeviceInfo {
    let description = [attrs.vendor.as_deref(), attrs.model.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    DeviceInfo {
        id: format!("/dev/{name}"),
        description: if description.is_empty() {
            "block device".to_string()
        } else {
            description
        },
        size_bytes: parse_attr::<u64>(attrs.size.as_deref()).map(|sectors| sectors * SIZE_UNIT),
        logical_sector_size: parse_attr(attrs.logical_block_size.as_deref()).unwrap_or(512),
        removability: classify(attrs.removable.as_deref()),
    }
}

fn parse_attr<T: std::str::FromStr>(value: Option<&str>) -> Option<T> {
    value?.trim().parse().ok()
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
        for id in [
            "/dev/sdb1",
            "mmcblk0p1",
            "/dev/nvme0n1p1",
            "/dev/vda1",
            "xvda2",
            "hda1",
        ] {
            assert!(device_name(id).is_err(), "{id}");
        }
        for id in ["/dev/nvme0n1", "vda", "xvda", "hda", "md0"] {
            assert!(device_name(id).is_ok(), "{id}");
        }
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

    #[test]
    fn virtual_devices_are_never_candidates() {
        for name in [
            "loop0", "ram3", "zram0", "dm-0", "md127", "sr0", "nbd0", "fd0",
        ] {
            assert!(!is_candidate(name), "{name}");
        }
        for name in ["sda", "sdb", "nvme0n1", "mmcblk0", "vda"] {
            assert!(is_candidate(name), "{name}");
        }
    }

    #[test]
    fn attributes_become_a_usable_device_identifier() {
        let info = device_info(
            "sda",
            &SysfsAttrs {
                size: Some("488397168\n".into()),
                logical_block_size: Some("512\n".into()),
                removable: Some("0\n".into()),
                vendor: Some("ATA     \n".into()),
                model: Some("Samsung SSD 840 Series\n".into()),
            },
        );

        assert_eq!(info.id, "/dev/sda");
        assert_eq!(device_name(&info.id).unwrap(), "sda");
        assert_eq!(info.size_bytes, Some(488_397_168 * 512));
        assert_eq!(info.logical_sector_size, 512);
        assert_eq!(info.removability, Removability::Fixed);
        assert!(
            info.description.contains("Samsung SSD 840 Series"),
            "{}",
            info.description
        );
    }

    /// `size` stays in 512B units even when the device reports 4096B blocks.
    /// Multiplying by the logical block size instead overstates it eightfold.
    #[test]
    fn size_is_counted_in_512_byte_units_regardless_of_block_size() {
        let info = device_info(
            "zram0",
            &SysfsAttrs {
                size: Some("16101016".into()),
                logical_block_size: Some("4096".into()),
                ..SysfsAttrs::default()
            },
        );

        assert_eq!(info.size_bytes, Some(16_101_016 * 512));
        assert_eq!(info.logical_sector_size, 4096);
    }

    #[test]
    fn missing_attributes_leave_the_device_unwritable() {
        let info = device_info("sdz", &SysfsAttrs::default());

        assert_eq!(info.size_bytes, None);
        assert_eq!(info.logical_sector_size, 512);
        assert_eq!(info.removability, Removability::Unknown);
        assert!(!info.removability.writable());
    }
}
