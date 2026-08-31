//! The read and write paths, expressed over `RawDevice` so they run identically
//! on every platform and in tests.

use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;

use crate::device::{DeviceInfo, RawDevice};
use crate::error::{DeviceError, Error, Result, Stage};
use crate::progress::Progress;
use crate::trace::Trace;

/// Transfer granularity. Always a multiple of any plausible sector size.
const CHUNK: usize = 1 << 20;

/// Chunks that may be in flight between the two sides of a transfer. Two is
/// enough to keep both busy; more only holds more memory.
const DEPTH: usize = 2;

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

/// The chunk math assumes the sector size divides `CHUNK`; any other value
/// would overrun the transfer buffers, so the device is refused up front.
fn require_usable_sector(info: &DeviceInfo) -> Result<()> {
    let sector = info.logical_sector_size;
    if sector == 0 || !sector.is_power_of_two() || sector as usize > CHUNK {
        return Err(Error::Device(DeviceError::new(
            Stage::QueryGeometry,
            format!(
                "{} reports logical sector size {sector}, which cannot chunk a \
                 transfer (need a power of two of at most {CHUNK})",
                info.id
            ),
        )));
    }
    Ok(())
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

/// Everything a range operation proves before it touches the device, shared
/// with `--dry-run` so a rehearsal approves exactly what the real run would.
/// Returns the exclusive end of the range.
pub fn check_range(info: &DeviceInfo, offset: u64, length: u64) -> Result<u64> {
    require_usable_sector(info)?;
    let sector = info.logical_sector_size;
    require_aligned("offset", offset, sector)?;
    let overflow =
        || Error::InvalidArgument(format!("offset {offset} + length {length} overflows"));
    let end = offset.checked_add(length).ok_or_else(overflow)?;
    let aligned = length
        .checked_next_multiple_of(u64::from(sector))
        .ok_or_else(overflow)?;
    ensure_within_device(info, offset, aligned)?;
    Ok(end)
}

/// What a read that only looks proves before it touches the device. Unlike a
/// transfer the offset need not be sector aligned: the sector the range starts
/// in is read whole and its head discarded, which is what lets an inspection
/// look at a structure that does not begin on a sector. Returns the exclusive
/// end of the range.
pub fn check_read_range(info: &DeviceInfo, offset: u64, length: u64) -> Result<u64> {
    require_usable_sector(info)?;
    let sector = u64::from(info.logical_sector_size);
    let overflow =
        || Error::InvalidArgument(format!("offset {offset} + length {length} overflows"));
    let end = offset.checked_add(length).ok_or_else(overflow)?;
    // The last sector is read whole, so it is the aligned end that has to fit.
    let aligned = end.checked_next_multiple_of(sector).ok_or_else(overflow)?;
    if let Some(size) = info.size_bytes
        && aligned > size
    {
        return Err(Error::InvalidArgument(format!(
            "range {offset}+{length} exceeds device size {size}"
        )));
    }
    Ok(end)
}

/// Reads `length` bytes from `offset` and hands them to `sink` in chunks, in
/// order. Returns the number of bytes handed over.
pub fn read_range(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    trace: &Trace,
    sink: &mut dyn FnMut(&[u8]) -> Result<()>,
) -> Result<u64> {
    check_read_range(device.info(), offset, length)?;
    let sector = device.info().logical_sector_size;
    let stride = u64::from(sector);
    // One sector of the buffer belongs to the head that gets discarded, so a
    // full chunk of wanted bytes still fits after the aligned read grows.
    let span = CHUNK as u64 - stride;
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;

    while done < length {
        let at = offset + done;
        let start = at - at % stride;
        let head = (at - start) as usize;
        let want = usize::try_from((length - done).min(span)).unwrap_or(CHUNK - head);
        let aligned =
            usize::try_from(align_up((head + want) as u64, sector)).unwrap_or_else(|_| head + want);

        device.read_at(start, &mut buf[..aligned]).map_err(|err| {
            trace.failed(format!("read {aligned}B at {start}"), &err);
            Error::Device(err)
        })?;
        trace.ok(Stage::Read, format!("read {aligned}B at {start}"), "ok");

        sink(&buf[head..head + want])?;
        done += want as u64;
    }
    Ok(done)
}

/// Returns the number of bytes written to `sink`.
pub fn dump(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    sink: &mut (dyn Write + Send),
    trace: &Trace,
    progress: &mut dyn Progress,
) -> Result<u64> {
    check_range(device.info(), offset, length)?;
    let sector = device.info().logical_sector_size;

    // The sink runs on its own thread so the next device read overlaps the write
    // of the chunk before it. The device stays here: `RawDevice` is not `Send`,
    // and neither is the trace it writes to.
    thread::scope(|scope| {
        let (filled, ready) = mpsc::sync_channel::<(Vec<u8>, usize)>(DEPTH);
        let (recycled, spare) = mpsc::channel::<Vec<u8>>();
        let refill = recycled.clone();

        let writer = scope.spawn(move || -> Result<()> {
            for (buf, want) in ready {
                sink.write_all(&buf[..want])
                    .map_err(|err| Error::io("writing output file", err))?;
                // Recycling is best effort: the other side may already be done
                // with this transfer, which is not a reason to stop writing.
                let _ = recycled.send(buf);
            }
            sink.flush()
                .map_err(|err| Error::io("flushing output file", err))
        });

        for _ in 0..DEPTH + 1 {
            let _ = refill.send(vec![0u8; CHUNK]);
        }
        drop(refill);

        let mut done = 0u64;
        let mut failure = None;
        while done < length {
            let want = usize::try_from(length - done).unwrap_or(CHUNK).min(CHUNK);
            let aligned = usize::try_from(align_up(want as u64, sector)).unwrap_or(want);
            let at = offset + done;

            // A disconnected spare channel means the writer stopped; its error
            // is the one worth reporting, so let the join produce it.
            let Ok(mut buf) = spare.recv() else { break };

            if let Err(err) = device.read_at(at, &mut buf[..aligned]) {
                trace.failed(format!("read {aligned}B at {at}"), &err);
                failure = Some(Error::Device(err));
                break;
            }
            trace.ok(Stage::Read, format!("read {aligned}B at {at}"), "ok");

            if filled.send((buf, want)).is_err() {
                break;
            }
            done += want as u64;
            progress.advance(done, length);
        }
        drop(filled);

        let written = writer.join().expect("output writer panicked");
        if let Some(err) = failure {
            return Err(err);
        }
        written?;
        progress.finish(done);
        Ok(done)
    })
}

/// On failure the last successfully written offset is carried in the error.
pub fn flash(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    source: &mut (dyn Read + Send),
    trace: &Trace,
    progress: &mut dyn Progress,
) -> Result<u64> {
    ensure_writable(device.info())?;
    check_range(device.info(), offset, length)?;
    let sector = device.info().logical_sector_size;

    // Mirror of dump: the source is read on its own thread so the file read of
    // the next chunk overlaps the device write of this one.
    thread::scope(|scope| {
        let (filled, ready) = mpsc::sync_channel::<std::io::Result<(Vec<u8>, usize)>>(DEPTH);
        let (recycled, spare) = mpsc::channel::<Vec<u8>>();
        for _ in 0..DEPTH + 1 {
            let _ = recycled.send(vec![0u8; CHUNK]);
        }

        scope.spawn(move || {
            let mut queued = 0u64;
            while queued < length {
                let want = usize::try_from(length - queued).unwrap_or(CHUNK).min(CHUNK);
                let Ok(mut buf) = spare.recv() else { return };
                let read = source.read_exact(&mut buf[..want]);
                let message = match read {
                    Ok(()) => Ok((buf, want)),
                    Err(err) => Err(err),
                };
                let failed = message.is_err();
                if filled.send(message).is_err() || failed {
                    return;
                }
                queued += want as u64;
            }
        });

        let mut done = 0u64;
        let mut last_flushed = 0u64;

        for message in ready {
            let (mut buf, want) = match message {
                Ok(chunk) => chunk,
                // Nothing written yet is a plain input failure; after that the
                // card is partially written, so the cache is pushed at the
                // medium and the error carries how much landed.
                Err(err) if done == 0 => {
                    return Err(Error::io("reading input file", err));
                }
                Err(err) => {
                    push(device, offset, done, trace)?;
                    let mut source = DeviceError::from_io(Stage::Read, &err);
                    source.message = format!("reading input file: {}", source.message);
                    return Err(abort(offset, done, source));
                }
            };
            let aligned = usize::try_from(align_up(want as u64, sector)).unwrap_or(want);
            let at = offset + done;

            // Only the sector the image ends inside is read back, so the
            // bytes past its end survive.
            if aligned > want {
                keep_tail(device, at, want, sector, &mut buf, trace)
                    .map_err(|err| abort(offset, done, err))?;
            }

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
            let _ = recycled.send(buf);
        }

        // What is left in the cache still has to cross to the medium, and on a
        // slow card that is most of the wall clock. Saying so beats a report
        // that reads as finished while the device is still working.
        progress.waiting("waiting for the medium");
        push(device, offset, done, trace)?;
        progress.finish(done);
        Ok(done)
    })
}

/// Reads the range back and compares it with `source`. Returns the number of
/// bytes that matched; the first difference aborts with its offset. Shares the
/// transfer guards so verify accepts exactly the ranges dump and flash do.
pub fn verify(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    source: &mut dyn Read,
    trace: &Trace,
    progress: &mut dyn Progress,
) -> Result<u64> {
    check_range(device.info(), offset, length)?;
    let sector = device.info().logical_sector_size;

    let mut from_device = vec![0u8; CHUNK];
    let mut from_file = vec![0u8; CHUNK];
    let mut done = 0u64;
    while done < length {
        let want = usize::try_from(length - done).unwrap_or(CHUNK).min(CHUNK);
        let aligned = usize::try_from(align_up(want as u64, sector)).unwrap_or(want);
        let at = offset + done;

        device
            .read_at(at, &mut from_device[..aligned])
            .map_err(|err| {
                trace.failed(format!("verify read {aligned}B at {at}"), &err);
                Error::Device(err)
            })?;
        trace.ok(Stage::Read, format!("verify read {aligned}B at {at}"), "ok");
        source
            .read_exact(&mut from_file[..want])
            .map_err(|e| Error::io("reading input file", e))?;

        // Equal chunks are the common case; the byte scan runs only on the
        // chunk that differs.
        if from_device[..want] != from_file[..want] {
            let i = (0..want)
                .position(|i| from_device[i] != from_file[i])
                .expect("the chunks differ");
            return Err(Error::VerifyFailed {
                offset: at + i as u64,
                expected: from_file[i],
                found: from_device[i],
            });
        }
        done += want as u64;
        progress.advance(done, length);
    }
    progress.finish(done);
    Ok(done)
}

/// Restores the bytes past the end of the image inside the sector it ends in,
/// so a length that is not a sector multiple leaves its neighbour intact. Only
/// that one sector is read, and its start is sector aligned because `at` is.
fn keep_tail(
    device: &mut dyn RawDevice,
    at: u64,
    want: usize,
    sector: u32,
    buf: &mut [u8],
    trace: &Trace,
) -> std::result::Result<(), DeviceError> {
    let aligned = usize::try_from(align_up(want as u64, sector)).unwrap_or(want);
    let sector = sector as usize;
    let tail_start = (want / sector) * sector;
    let from = at + tail_start as u64;
    let mut tail = vec![0u8; aligned - tail_start];

    match device.read_at(from, &mut tail) {
        Ok(_) => {
            trace.ok(
                Stage::Read,
                format!("read-back {}B at {from}", tail.len()),
                "tail sector preserved",
            );
            buf[want..aligned].copy_from_slice(&tail[want - tail_start..]);
            Ok(())
        }
        Err(err) => {
            trace.failed(format!("read-back {}B at {from}", tail.len()), &err);
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

    /// `start` is where the transfer began, whichever step failed; a recovery
    /// that reads `start + written` must not be pointed at the failing chunk.
    #[test]
    fn a_tail_read_back_failure_reports_the_transfer_start() {
        let mut device = MemoryDevice::new("mem0", 4 << 20, Removability::Removable);
        device.fail_reads_from(4096 + (1 << 20));
        let data = pattern((1 << 20) + 100);

        let err = flash(
            &mut device,
            4096,
            data.len() as u64,
            &mut data.as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        match err {
            Error::WriteAborted { start, written, .. } => {
                assert_eq!(start, 4096);
                assert_eq!(written, 1 << 20);
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    /// Errors from the far side of the transfer have to survive the handoff.
    #[test]
    fn dump_reports_a_sink_that_refuses_the_data() {
        struct Refuses;
        impl Write for Refuses {
            fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::from(std::io::ErrorKind::StorageFull))
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut device = MemoryDevice::new("mem0", 8192, Removability::Removable);
        let err = dump(
            &mut device,
            0,
            4096,
            &mut Refuses,
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        assert!(matches!(err, Error::Io { .. }), "{err}");
        assert_eq!(err.exit_code(), 3);
    }

    /// The card holds every chunk that landed before the source died, so the
    /// error has to say how much, and the cache has to be pushed at the medium
    /// so that count describes the card rather than memory.
    #[test]
    fn a_source_failure_mid_flash_reports_and_flushes_what_landed() {
        struct DiesAfter {
            data: Vec<u8>,
            served: usize,
        }
        impl Read for DiesAfter {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.served >= self.data.len() {
                    return Err(std::io::Error::from(std::io::ErrorKind::TimedOut));
                }
                let n = buf.len().min(self.data.len() - self.served);
                buf[..n].copy_from_slice(&self.data[self.served..self.served + n]);
                self.served += n;
                Ok(n)
            }
        }

        let mut device = MemoryDevice::new("mem0", 4 << 20, Removability::Removable);
        let mut source = DiesAfter {
            data: pattern(1 << 20),
            served: 0,
        };

        let err = flash(
            &mut device,
            4096,
            2 << 20,
            &mut source,
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
                assert_eq!(start, 4096);
                assert_eq!(written, 1 << 20);
                assert!(source.message.contains("reading input file"), "{source}");
            }
            other => panic!("unexpected error: {other}"),
        }
        assert_eq!(device.flushes(), 1);
    }

    #[test]
    fn flash_reports_a_source_that_ends_early() {
        let mut device = MemoryDevice::new("mem0", 8192, Removability::Removable);
        let short = pattern(512);

        let err = flash(
            &mut device,
            0,
            4096,
            &mut short.as_slice(),
            &Trace::new(),
            &mut Silent,
        )
        .unwrap_err();

        assert!(matches!(err, Error::Io { .. }), "{err}");
    }

    /// The chunk math holds only for sector sizes that divide the chunk; a
    /// device reporting anything else must be refused, not overrun a buffer.
    #[test]
    fn a_sector_size_that_cannot_chunk_is_refused_not_a_panic() {
        for sector in [0u32, 520, 4224, (CHUNK as u32) * 2] {
            let mut device = MemoryDevice::new("mem0", 4 << 20, Removability::Removable);
            device.set_sector_size(sector);

            let err = dump(
                &mut device,
                0,
                2 << 20,
                &mut Vec::new(),
                &Trace::new(),
                &mut Silent,
            )
            .unwrap_err();

            assert!(matches!(err, Error::Device(_)), "sector {sector}: {err}");
            assert!(err.to_string().contains("sector"), "sector {sector}: {err}");
        }
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

    /// An inspection has to be able to start where a structure starts, not
    /// where the sector does.
    #[test]
    fn a_read_range_starts_where_it_was_asked_to_start() {
        let mut device = MemoryDevice::new("mem0", 8192, Removability::Removable);
        let data = pattern(8192);
        device.contents_mut().copy_from_slice(&data);

        let mut seen = Vec::new();
        let n = read_range(&mut device, 100, 300, &Trace::new(), &mut |bytes| {
            seen.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();

        assert_eq!(n, 300);
        assert_eq!(seen, data[100..400]);
    }

    /// A range longer than one chunk arrives in order and unbroken.
    #[test]
    fn a_read_range_longer_than_a_chunk_arrives_in_order() {
        let size = 3 * CHUNK;
        let mut device = MemoryDevice::new("mem0", size, Removability::Removable);
        let data = pattern(size);
        device.contents_mut().copy_from_slice(&data);

        let want = 2 * CHUNK as u64 + 4096;
        let mut seen = Vec::new();
        let n = read_range(&mut device, 512, want, &Trace::new(), &mut |bytes| {
            seen.extend_from_slice(bytes);
            Ok(())
        })
        .unwrap();

        assert_eq!(n, want);
        assert_eq!(seen, data[512..512 + want as usize]);
    }

    #[test]
    fn a_read_range_past_the_device_end_is_refused_before_any_read() {
        let mut device = MemoryDevice::new("mem0", 4096, Removability::Removable);
        device.fail_reads_from(0);

        let err = read_range(&mut device, 3900, 512, &Trace::new(), &mut |_| Ok(())).unwrap_err();

        assert!(matches!(err, Error::InvalidArgument(_)), "{err}");
    }

    /// The rehearsal has to approve exactly what the real read would, including
    /// the unaligned start a transfer refuses.
    #[test]
    fn the_read_check_accepts_an_unaligned_offset_and_reports_the_end() {
        let device = MemoryDevice::new("mem0", 4096, Removability::Removable);

        assert_eq!(check_read_range(device.info(), 100, 300).unwrap(), 400);
        assert!(check_read_range(device.info(), 3900, 512).is_err());
    }
}
