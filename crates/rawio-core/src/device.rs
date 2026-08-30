//! Device abstraction. Everything above this layer is platform independent and
//! testable on any host, including hosts that have no device access.

use crate::error::{DeviceError, Stage};

/// Only `Removable` may be written. `Unknown` is treated as fixed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removability {
    Removable,
    Fixed,
    Unknown,
}

impl Removability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Removability::Removable => "removable",
            Removability::Fixed => "fixed",
            Removability::Unknown => "unknown",
        }
    }

    pub const fn writable(self) -> bool {
        matches!(self, Removability::Removable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Identifier accepted verbatim as the `<device>` argument.
    pub id: String,
    pub description: String,
    pub size_bytes: Option<u64>,
    pub logical_sector_size: u32,
    pub removability: Removability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

/// Positioned raw access. Offsets and buffer lengths are sector aligned by the
/// caller, because Windows physical-disk handles may require it.
pub trait RawDevice {
    fn info(&self) -> &DeviceInfo;
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError>;
    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, DeviceError>;
    fn flush(&mut self) -> Result<(), DeviceError>;
}

pub trait Backend {
    fn enumerate(&self) -> Result<Vec<DeviceInfo>, DeviceError>;
    fn open(&self, id: &str, access: Access) -> Result<Box<dyn RawDevice>, DeviceError>;
}

/// In-memory device backing the transfer, alignment and PIT tests.
#[derive(Debug)]
pub struct MemoryDevice {
    info: DeviceInfo,
    data: Vec<u8>,
    /// Offset at which the next write fails, simulating a vanished device.
    fail_write_at: Option<u64>,
}

impl MemoryDevice {
    pub fn new(id: &str, size: usize, removability: Removability) -> Self {
        Self {
            info: DeviceInfo {
                id: id.to_string(),
                description: "in-memory test device".to_string(),
                size_bytes: Some(size as u64),
                logical_sector_size: 512,
                removability,
            },
            data: vec![0; size],
            fail_write_at: None,
        }
    }

    pub fn fail_writes_from(&mut self, offset: u64) {
        self.fail_write_at = Some(offset);
    }

    pub fn contents(&self) -> &[u8] {
        &self.data
    }

    pub fn contents_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    fn range(&self, offset: u64, len: usize) -> Result<std::ops::Range<usize>, DeviceError> {
        let start = usize::try_from(offset)
            .map_err(|_| DeviceError::new(Stage::Seek, format!("offset {offset} out of range")))?;
        let end = start
            .checked_add(len)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| {
                DeviceError::new(
                    Stage::Seek,
                    format!(
                        "range {offset}+{len} exceeds device size {}",
                        self.data.len()
                    ),
                )
            })?;
        Ok(start..end)
    }
}

impl RawDevice for MemoryDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let range = self.range(offset, buf.len())?;
        buf.copy_from_slice(&self.data[range]);
        Ok(buf.len())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, DeviceError> {
        if self.fail_write_at.is_some_and(|fail| offset >= fail) {
            return Err(DeviceError::with_os_error(
                Stage::Write,
                "device removed",
                433,
            ));
        }
        let range = self.range(offset, buf.len())?;
        self.data[range].copy_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}
