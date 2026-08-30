//! Win32 calls. Compiled only on Windows and unverified until it runs on real
//! hardware; every buffer it retrieves is handed straight to `logic` to parse.

use std::fs::{File, OpenOptions};
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::System::IO::DeviceIoControl;

use super::logic;
use crate::device::{Access, DeviceInfo, RawDevice, Removability, VolumeLock};
use crate::error::{DeviceError, Stage};
use crate::trace::Trace;

/// Physical drive numbers are dense in practice, but an unplugged device leaves
/// a hole, so the scan runs the whole range instead of stopping at the first miss.
const MAX_DRIVES: u32 = 32;

/// `STORAGE_DEVICE_DESCRIPTOR` is variable length; the ids sit past the fixed part.
const DESCRIPTOR_BUFFER: usize = 1024;

pub fn enumerate(trace: &Trace) -> Result<Vec<DeviceInfo>, DeviceError> {
    let mut devices = Vec::new();
    for index in 0..MAX_DRIVES {
        let path = logic::physical_drive_path(index);
        // Zero desired access opens the device for queries only, which needs no
        // elevation, so listing works from an ordinary shell.
        let Ok(handle) = OpenOptions::new().access_mode(0).open(&path) else {
            continue;
        };
        trace.ok(Stage::Enumerate, &path, "present");
        devices.push(describe(index, &handle, trace));
    }
    Ok(devices)
}

pub fn open(index: u32, access: Access, trace: &Trace) -> Result<Box<dyn RawDevice>, DeviceError> {
    let path = logic::physical_drive_path(index);
    let write = matches!(access, Access::ReadWrite);

    let file = OpenOptions::new()
        .read(true)
        .write(write)
        .open(&path)
        .map_err(|err| {
            let err = DeviceError::from_io(Stage::Open, &err);
            trace.failed(&path, &err);
            err
        })?;
    trace.ok(
        Stage::Open,
        &path,
        if write { "read-write" } else { "read-only" },
    );

    let info = describe(index, &file, trace);

    // Never lock volumes on a device the caller is about to be refused.
    let locks = if write && info.removability.writable() {
        // Dismounting invalidates what the OS has cached about the filesystem.
        // Without it a raw write under a mounted volume leaves Windows holding
        // metadata that no longer describes the medium.
        lock_volumes_on(index, trace, Dismount::Yes).0
    } else {
        Vec::new()
    };

    Ok(Box::new(WindowsDevice {
        info,
        file,
        _locks: locks,
    }))
}

fn describe(index: u32, file: &File, trace: &Trace) -> DeviceInfo {
    let path = logic::physical_drive_path(index);

    let descriptor = control(
        file,
        logic::IOCTL_STORAGE_QUERY_PROPERTY,
        &logic::STORAGE_DEVICE_PROPERTY_QUERY,
        DESCRIPTOR_BUFFER,
        Stage::QueryGeometry,
    )
    .inspect_err(|err| trace.failed(format!("{path} STORAGE_QUERY_PROPERTY"), err))
    .ok()
    .and_then(|buf| logic::parse_device_descriptor(&buf));

    let size_bytes = control(
        file,
        logic::IOCTL_DISK_GET_LENGTH_INFO,
        &[],
        8,
        Stage::QueryGeometry,
    )
    .inspect_err(|err| trace.failed(format!("{path} DISK_GET_LENGTH_INFO"), err))
    .ok()
    .and_then(|buf| {
        buf.get(..8)
            .map(|len| u64::from_le_bytes(len.try_into().unwrap()))
    });

    let logical_sector_size = control(
        file,
        logic::IOCTL_DISK_GET_DRIVE_GEOMETRY,
        &[],
        24,
        Stage::QueryGeometry,
    )
    .inspect_err(|err| trace.failed(format!("{path} DISK_GET_DRIVE_GEOMETRY"), err))
    .ok()
    .and_then(|buf| logic::parse_bytes_per_sector(&buf))
    .unwrap_or(512);

    let removability = descriptor.as_ref().map_or(Removability::Unknown, |it| {
        logic::classify(it.removable_media, it.bus_type)
    });

    let description = descriptor
        .as_ref()
        .map(|it| format!("{} {}", it.vendor, it.product).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "physical disk".to_string());

    trace.ok(
        Stage::QueryGeometry,
        &path,
        format!("{removability:?} sector={logical_sector_size} size={size_bytes:?}"),
    );

    DeviceInfo {
        id: path,
        description,
        size_bytes,
        logical_sector_size,
        removability,
    }
}

/// Rehearses the write path and undoes it: a writable handle plus the volume
/// locks a write would need, then both released. Nothing is written.
pub fn rehearse_write(index: u32, trace: &Trace) -> Result<Vec<VolumeLock>, DeviceError> {
    let path = logic::physical_drive_path(index);
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|err| {
            let err = DeviceError::from_io(Stage::Open, &err);
            trace.failed(&path, &err);
            err
        })?;
    trace.ok(Stage::Open, &path, "read-write, for rehearsal only");

    let (locks, report) = lock_volumes_on(index, trace, Dismount::No);
    drop(locks);
    drop(handle);
    Ok(report)
}

/// Whether the volume is also taken offline, which a real write needs and a
/// rehearsal must not do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dismount {
    Yes,
    No,
}

/// A write through a physical disk handle is refused where a mounted volume
/// covers it, so every volume on this disk is locked first and stays locked for
/// as long as its handle is open. A failure here is not fatal: an unformatted
/// card has nothing to lock, and the write may still succeed.
fn lock_volumes_on(disk: u32, trace: &Trace, dismount: Dismount) -> (Vec<File>, Vec<VolumeLock>) {
    let mut locked = Vec::new();
    let mut report = Vec::new();
    for letter in 'A'..='Z' {
        let path = logic::volume_path(letter);
        let Ok(volume) = OpenOptions::new().read(true).write(true).open(&path) else {
            continue;
        };
        let Ok(extents) = control(
            &volume,
            logic::IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            &[],
            256,
            Stage::QueryGeometry,
        ) else {
            continue;
        };
        if !logic::parse_disk_extents(&extents).contains(&disk) {
            continue;
        }
        match control(&volume, logic::FSCTL_LOCK_VOLUME, &[], 0, Stage::LockVolume) {
            Ok(_) => {
                trace.ok(Stage::LockVolume, &path, "locked");
                if dismount == Dismount::Yes {
                    match control(
                        &volume,
                        logic::FSCTL_DISMOUNT_VOLUME,
                        &[],
                        0,
                        Stage::LockVolume,
                    ) {
                        Ok(_) => trace.ok(Stage::LockVolume, &path, "dismounted"),
                        Err(err) => trace.failed(format!("{path} dismount"), &err),
                    }
                }
                report.push(VolumeLock {
                    volume: path,
                    locked: true,
                    error: None,
                });
                locked.push(volume);
            }
            Err(err) => {
                trace.failed(&path, &err);
                report.push(VolumeLock {
                    volume: path,
                    locked: false,
                    error: Some(err),
                });
            }
        }
    }
    (locked, report)
}

fn control(
    file: &File,
    code: u32,
    input: &[u8],
    out_len: usize,
    stage: Stage,
) -> Result<Vec<u8>, DeviceError> {
    let mut out = vec![0u8; out_len];
    let mut returned = 0u32;

    // SAFETY: the handle is borrowed from `file` and outlives the call; the two
    // buffers are distinct, and the lengths passed are their real lengths.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if out.is_empty() {
                std::ptr::null_mut()
            } else {
                out.as_mut_ptr().cast()
            },
            out_len as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };

    if ok == 0 {
        let err = std::io::Error::last_os_error();
        let mut err = DeviceError::from_io(stage, &err);
        if let Some(code) = err.os_error {
            err.message = format!("{} - {}", err.message, logic::describe_error(code));
        }
        return Err(err);
    }
    out.truncate(returned as usize);
    Ok(out)
}

struct WindowsDevice {
    info: DeviceInfo,
    file: File,
    /// Volume locks last exactly as long as their handles.
    _locks: Vec<File>,
}

impl RawDevice for WindowsDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        let mut done = 0;
        while done < buf.len() {
            let at = offset + done as u64;
            match self.file.seek_read(&mut buf[done..], at) {
                Ok(0) => {
                    return Err(DeviceError::new(Stage::Read, format!("short read at {at}")));
                }
                Ok(n) => done += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(DeviceError::from_io(Stage::Read, &err)),
            }
        }
        Ok(done)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, DeviceError> {
        let mut done = 0;
        while done < buf.len() {
            let at = offset + done as u64;
            match self.file.seek_write(&buf[done..], at) {
                Ok(0) => {
                    return Err(DeviceError::new(
                        Stage::Write,
                        format!("short write at {at}"),
                    ));
                }
                Ok(n) => done += n,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
                Err(err) => return Err(DeviceError::from_io(Stage::Write, &err)),
            }
        }
        Ok(done)
    }

    fn flush(&mut self) -> Result<(), DeviceError> {
        self.file
            .sync_all()
            .map_err(|err| DeviceError::from_io(Stage::Flush, &err))
    }
}
