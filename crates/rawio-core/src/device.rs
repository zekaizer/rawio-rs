//! Device abstraction. Everything above this layer is platform independent and
//! testable on any host, including hosts that have no device access.

use crate::error::{DeviceError, Stage};
use crate::trace::Trace;

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

/// What stands between a write and the medium on one volume the OS has mounted
/// over the target device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeLock {
    /// How the OS names the volume, in the form the user will recognise.
    pub volume: String,
    pub locked: bool,
    /// Why it could not be locked, when it could not.
    pub error: Option<DeviceError>,
}

/// The trace is passed in because opening a device is itself several steps, and
/// which one failed is the whole point of the report.
pub trait Backend {
    fn enumerate(&self, trace: &Trace) -> Result<Vec<DeviceInfo>, DeviceError>;
    fn open(
        &self,
        id: &str,
        access: Access,
        trace: &Trace,
    ) -> Result<Box<dyn RawDevice>, DeviceError>;

    /// Runs the write path as far as it can without writing: takes a writable
    /// handle, tries the volume locks a write would need, then releases both.
    /// The point is to learn on site whether a write would be permitted without
    /// risking a card to find out.
    fn rehearse_write(&self, id: &str, trace: &Trace) -> Result<Vec<VolumeLock>, DeviceError> {
        let _ = (id, trace);
        Ok(Vec::new())
    }
}

/// In-memory device backing the transfer, alignment and PIT tests.
#[derive(Debug)]
pub struct MemoryDevice {
    info: DeviceInfo,
    data: Vec<u8>,
    /// Offset at which the next write fails, simulating a vanished device.
    fail_write_at: Option<u64>,
    /// Offset at which the next read fails, simulating a vanished device.
    fail_read_at: Option<u64>,
    flushes: usize,
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
            fail_read_at: None,
            flushes: 0,
        }
    }

    pub fn flushes(&self) -> usize {
        self.flushes
    }

    pub fn fail_writes_from(&mut self, offset: u64) {
        self.fail_write_at = Some(offset);
    }

    pub fn fail_reads_from(&mut self, offset: u64) {
        self.fail_read_at = Some(offset);
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
        if self.fail_read_at.is_some_and(|fail| offset >= fail) {
            return Err(DeviceError::with_os_error(
                Stage::Read,
                "device removed",
                433,
            ));
        }
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
        self.flushes += 1;
        Ok(())
    }
}
