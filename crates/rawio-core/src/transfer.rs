//! The read and write paths, expressed over `RawDevice` so they run identically
//! on every platform and in tests.

use std::io::{Read, Write};

use crate::device::{DeviceInfo, RawDevice};
use crate::error::{DeviceError, Error, Result, Stage};
use crate::progress::Progress;
use crate::trace::Trace;

/// Transfer granularity. Always a multiple of any plausible sector size.
const CHUNK: usize = 1 << 20;

/// How much may sit in the page cache before it is pushed at the medium.
///
/// Without this the whole image is absorbed at memory speed and the single
/// flush at the end blocks for as long as the card needs, so the progress
/// report races to 100% and then appears to hang. Flushing as we go costs
/// nothing on a device that is already the bottleneck.
pub const FLUSH_INTERVAL: u64 = 32 << 20;

/// True when enough has been written since the last flush to push again.
pub fn flush_due(done: u64, last_flushed: u64) -> bool {
    done.saturating_sub(last_flushed) >= FLUSH_INTERVAL
}

pub fn align_up(value: u64, sector: u32) -> u64 {
    let sector = u64::from(sector);
    value.div_ceil(sector) * sector
}

pub fn require_aligned(label: &str, value: u64, sector: u32) -> Result<()> {
    if value % u64::from(sector) != 0 {
        return Err(Error::InvalidArgument(format!(
            "{label} {value} is not a multiple of the logical sector size {sector}"
        )));
    }
    Ok(())
}

/// No argument reaches this check, so there is nothing to override.
pub fn ensure_writable(info: &DeviceInfo) -> Result<()> {
    if info.removability.writable() {
        return Ok(());
    }
    Err(Error::NotRemovable {
        device: info.id.clone(),
        classification: info.removability.as_str(),
    })
}

fn ensure_within_device(info: &DeviceInfo, offset: u64, length: u64) -> Result<()> {
    let Some(size) = info.size_bytes else {
        return Ok(());
    };
    let end = offset.checked_add(length).ok_or_else(|| {
        Error::InvalidArgument(format!("offset {offset} + length {length} overflows"))
    })?;
    if end > size {
        return Err(Error::InvalidArgument(format!(
            "range {offset}+{length} exceeds device size {size}"
        )));
    }
    Ok(())
}

/// Returns the number of bytes written to `sink`.
pub fn dump(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    sink: &mut dyn Write,
    trace: &Trace,
    progress: &mut dyn Progress,
) -> Result<u64> {
    let sector = device.info().logical_sector_size;
    require_aligned("offset", offset, sector)?;
    ensure_within_device(device.info(), offset, align_up(length, sector))?;

    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    while done < length {
        let want = usize::try_from(length - done).unwrap_or(CHUNK).min(CHUNK);
        let aligned = usize::try_from(align_up(want as u64, sector)).unwrap_or(want);
        let at = offset + done;
        let chunk = &mut buf[..aligned];
        device.read_at(at, chunk).map_err(|err| {
            trace.failed(format!("read {aligned}B at {at}"), &err);
            Error::Device(err)
        })?;
        trace.ok(Stage::Read, format!("read {aligned}B at {at}"), "ok");
        sink.write_all(&chunk[..want])
            .map_err(|err| Error::io("writing output file", err))?;
        done += want as u64;
        progress.advance(done, length);
    }
    sink.flush()
        .map_err(|err| Error::io("flushing output file", err))?;
    progress.finish(done);
    Ok(done)
}

/// On failure the last successfully written offset is carried in the error.
pub fn flash(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    source: &mut dyn Read,
    trace: &Trace,
    progress: &mut dyn Progress,
) -> Result<u64> {
    ensure_writable(device.info())?;
    let sector = device.info().logical_sector_size;
    require_aligned("offset", offset, sector)?;
    ensure_within_device(device.info(), offset, align_up(length, sector))?;

    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    let mut last_flushed = 0u64;
    while done < length {
        let want = usize::try_from(length - done).unwrap_or(CHUNK).min(CHUNK);
        let aligned = usize::try_from(align_up(want as u64, sector)).unwrap_or(want);
        let at = offset + done;

        // The tail sector is read first so padding preserves whatever follows the image.
        if aligned > want {
            read_back(device, at, &mut buf[..aligned], trace)
                .map_err(|err| abort(at, done, err))?;
        }
        source
            .read_exact(&mut buf[..want])
            .map_err(|err| Error::io("reading input file", err))?;

        device.write_at(at, &buf[..aligned]).map_err(|err| {
            trace.failed(format!("write {aligned}B at {at}"), &err);
            abort(offset, done, err)
        })?;
        trace.ok(Stage::Write, format!("write {aligned}B at {at}"), "ok");
        done += want as u64;
        progress.advance(done, length);

        if flush_due(done, last_flushed) {
            push(device, offset, done, trace)?;
            last_flushed = done;
        }
    }

    // What is left in the cache still has to cross to the medium, and on a slow
    // card that is most of the wall clock. Saying so beats a report that reads
    // as finished while the device is still working.
    progress.waiting("waiting for the medium");
    push(device, offset, done, trace)?;
    progress.finish(done);
    Ok(done)
}

fn read_back(
    device: &mut dyn RawDevice,
    at: u64,
    buf: &mut [u8],
    trace: &Trace,
) -> std::result::Result<(), DeviceError> {
    let len = buf.len();
    match device.read_at(at, buf) {
        Ok(_) => {
            trace.ok(
                Stage::Read,
                format!("read-back {len}B at {at}"),
                "tail sector preserved",
            );
            Ok(())
        }
        Err(err) => {
            trace.failed(format!("read-back {len}B at {at}"), &err);
            Err(err)
        }
    }
}

fn push(device: &mut dyn RawDevice, start: u64, done: u64, trace: &Trace) -> Result<()> {
    device.flush().map_err(|err| {
        trace.failed(format!("flush after {done}B"), &err);
        abort(start, done, err)
    })?;
    trace.ok(Stage::Flush, format!("flush after {done}B"), "ok");
    Ok(())
}

fn abort(start: u64, written: u64, source: DeviceError) -> Error {
    Error::WriteAborted {
        start,
        written,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{MemoryDevice, Removability};
    use crate::progress::Silent;

    /// Records the order the transfer reported things in.
    #[derive(Default)]
    struct Recorder {
        events: Vec<String>,
    }

    impl Progress for Recorder {
        fn advance(&mut self, done: u64, total: u64) {
            self.events.push(format!("advance {done}/{total}"));
        }

        fn waiting(&mut self, what: &str) {
            self.events.push(format!("waiting {what}"));
        }

        fn finish(&mut self, done: u64) {
            self.events.push(format!("finish {done}"));
        }
    }

    #[test]
    fn the_flush_interval_is_measured_from_the_last_flush() {
        assert!(!flush_due(0, 0));
        assert!(!flush_due(FLUSH_INTERVAL - 1, 0));
        assert!(flush_due(FLUSH_INTERVAL, 0));
        assert!(!flush_due(FLUSH_INTERVAL + 1, FLUSH_INTERVAL));
        assert!(flush_due(2 * FLUSH_INTERVAL, FLUSH_INTERVAL));
    }

    /// The page cache takes the write long before the medium does, so the
    /// report has to say it is still waiting rather than claim it is done.
    #[test]
    fn flash_reports_the_wait_for_the_medium_before_it_finishes() {
        let mut device = MemoryDevice::new("mem0", 8192, Removability::Removable);
        let mut recorder = Recorder::default();

        flash(
            &mut device,
            0,
            2048,
            &mut pattern(2048).as_slice(),
            &Trace::new(),
            &mut recorder,
        )
        .unwrap();

        let waiting = recorder
            .events
            .iter()
            .position(|e| e.starts_with("waiting"));
        let finish = recorder.events.iter().position(|e| e.starts_with("finish"));
        assert!(waiting.is_some(), "{:?}", recorder.events);
        assert!(waiting < finish, "{:?}", recorder.events);
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn dump_returns_exact_bytes_at_offset() {
        let mut device = MemoryDevice::new("mem0", 8192, Removability::Removable);
        let data = pattern(1024);
        device.contents_mut()[512..1536].copy_from_slice(&data);

        let mut out = Vec::new();
        let n = dump(&mut device, 512, 1024, &mut out, &Trace::new(), &mut Silent).unwrap();

        assert_eq!(n, 1024);
        assert_eq!(out, data);
    }

    #[test]
    fn flash_then_dump_round_trips() {
        let mut device = MemoryDevice::new("mem0", 8192, Removability::Removable);
        let data = pattern(2048);

        flash(
            &mut device,
            1024,
            2048,
            &mut data.as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap();
        let mut out = Vec::new();
        dump(
            &mut device,
            1024,
            2048,
            &mut out,
            &Trace::new(),
            &mut Silent,
        )
        .unwrap();

        assert_eq!(out, data);
    }

    #[test]
    fn flash_preserves_bytes_after_an_unaligned_tail() {
        let mut device = MemoryDevice::new("mem0", 4096, Removability::Removable);
        device.contents_mut()[600..1024].fill(0xAA);

        flash(
            &mut device,
            0,
            600,
            &mut pattern(600).as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap();

        assert_eq!(&device.contents()[..600], pattern(600).as_slice());
        assert!(device.contents()[600..1024].iter().all(|b| *b == 0xAA));
    }

    #[test]
    fn flash_refuses_a_fixed_device() {
        let mut device = MemoryDevice::new("disk0", 4096, Removability::Fixed);
        let err = flash(
            &mut device,
            0,
            512,
            &mut pattern(512).as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        assert!(matches!(err, Error::NotRemovable { .. }));
        assert_eq!(err.exit_code(), 4);
        assert!(device.contents().iter().all(|b| *b == 0));
    }

    #[test]
    fn flash_rejects_input_larger_than_the_device() {
        let mut device = MemoryDevice::new("mem0", 1024, Removability::Removable);
        let err = flash(
            &mut device,
            512,
            4096,
            &mut pattern(4096).as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        assert!(matches!(err, Error::InvalidArgument(_)));
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn flash_reports_the_last_written_offset_when_the_device_vanishes() {
        let mut device = MemoryDevice::new("mem0", 4 << 20, Removability::Removable);
        device.fail_writes_from(1 << 20);
        let data = pattern(2 << 20);

        let err = flash(
            &mut device,
            0,
            2 << 20,
            &mut data.as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        match err {
            Error::WriteAborted {
                start,
                written,
                ref source,
            } => {
                assert_eq!(start, 0);
                assert_eq!(written, 1 << 20);
                assert_eq!(source.os_error, Some(433));
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(err.exit_code(), 5);
    }

    #[test]
    fn unaligned_offset_is_rejected() {
        let mut device = MemoryDevice::new("mem0", 4096, Removability::Removable);
        let err = dump(
            &mut device,
            100,
            512,
            &mut Vec::new(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        assert!(matches!(err, Error::InvalidArgument(_)));
    }
}
