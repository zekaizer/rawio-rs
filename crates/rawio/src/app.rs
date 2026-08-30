//! Command dispatch. Takes the backend as an argument so the whole flow can be
//! driven by a fake device in tests.

use std::fs::File;
use std::io::{Read, Write};

use crate::cli::{
    Cli, Command, DumpArgs, FlashArgs, Location, PitArgs, PitSource, ProbeArgs, TransferOptions,
    VerifyArgs,
};
use crate::longpath;
use crate::progress::{Bar, human_size};
use rawio_core::device::{Access, Backend, DeviceInfo, RawDevice};
use rawio_core::error::{Error, Result, Stage};
use rawio_core::pit::{ASSUMED_BLOCK_SIZE, Pit};
use rawio_core::progress::{Progress, Silent};
use rawio_core::trace::Trace;
use rawio_core::transfer;

/// Settings that apply to every command, lifted out of the parsed arguments so
/// the handlers do not each grow another parameter.
struct Options {
    pit_at: u64,
    dry_run: bool,
    progress: bool,
}

impl Options {
    /// A command that moves bytes: it can be rehearsed and it can report.
    fn transfer(pit: &PitSource, transfer: &TransferOptions) -> Self {
        Self {
            pit_at: pit.pit_offset,
            dry_run: transfer.dry_run,
            progress: Bar::enabled(transfer.no_progress),
        }
    }

    /// A command that only looks: neither rehearsal nor progress applies.
    fn inspect(pit: &PitSource) -> Self {
        Self {
            pit_at: pit.pit_offset,
            dry_run: false,
            progress: false,
        }
    }

    fn bar(&self, label: &'static str) -> Box<dyn Progress> {
        if self.progress {
            Box::new(Bar::new(label))
        } else {
            Box::new(Silent)
        }
    }
}

/// How the caller named the partition it wants.
enum Selector<'a> {
    Name(&'a str),
    Id(u32),
}

/// Resolved target range. `length` is absent when only the caller knows it
/// (a `flash` whose length comes from the input file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub offset: u64,
    pub length: Option<u64>,
}

pub fn run(cli: &Cli, backend: &dyn Backend, out: &mut dyn Write, trace: &Trace) -> Result<()> {
    match &cli.command {
        Command::List => list(backend, out, trace),
        Command::Probe(args) => probe(
            args,
            backend,
            out,
            trace,
            &Options::inspect(&args.pit_source),
        ),
        Command::Pit(args) => pit(
            args,
            backend,
            out,
            trace,
            &Options::inspect(&args.pit_source),
        ),
        Command::Dump(args) => {
            let opts = Options::transfer(&args.pit_source, &args.transfer);
            dump(args, backend, out, trace, &opts)
        }
        Command::Flash(args) => {
            let opts = Options::transfer(&args.pit_source, &args.transfer);
            flash(args, backend, out, trace, &opts)
        }
        Command::Verify(args) => {
            let opts = Options::transfer(&args.pit_source, &args.transfer);
            verify(args, backend, out, trace, &opts)
        }
    }
}

fn list(backend: &dyn Backend, out: &mut dyn Write, trace: &Trace) -> Result<()> {
    let devices = backend.enumerate(trace)?;
    if devices.is_empty() {
        writeln!(out, "no devices found").map_err(|e| Error::io("writing output", e))?;
        return Ok(());
    }
    for info in &devices {
        writeln!(out, "{}", describe(info)).map_err(|e| Error::io("writing output", e))?;
    }
    Ok(())
}

/// Collects everything an on-site check needs, touching nothing.
fn probe(
    args: &ProbeArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let devices = backend.enumerate(trace)?;
    for info in &devices {
        writeln!(out, "device: {}", describe(info)).map_err(|e| Error::io("writing output", e))?;
    }

    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let info = device.info().clone();
    writeln!(out, "target: {}", describe(&info)).map_err(|e| Error::io("writing output", e))?;
    writeln!(out, "writable: {}", info.removability.writable())
        .map_err(|e| Error::io("writing output", e))?;
    rehearse(args, backend, out, trace, &info)?;

    if args.pit {
        let table = read_pit(&mut *device, opts.pit_at, trace)?;
        print_table(out, &table, opts.pit_at, &info)?;
    }

    if let Some(range) = resolve(&mut *device, &args.location, None, out, trace, opts)? {
        writeln!(
            out,
            "resolved: offset={} length={:?}",
            range.offset, range.length
        )
        .map_err(|e| Error::io("writing output", e))?;
    }
    Ok(())
}

/// Runs the write path as far as it goes without writing, so the answer to
/// "would a flash be permitted here" does not cost a card to find out.
fn rehearse(
    args: &ProbeArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
    info: &DeviceInfo,
) -> Result<()> {
    let io = |e| Error::io("writing output", e);

    if !info.removability.writable() {
        return writeln!(
            out,
            "write rehearsal: skipped, {} is not removable and would be refused",
            info.id
        )
        .map_err(io);
    }

    match backend.rehearse_write(&args.device, trace) {
        Err(err) => writeln!(out, "write rehearsal: no writable handle - {err}").map_err(io),
        Ok(volumes) if volumes.is_empty() => writeln!(
            out,
            "write rehearsal: writable handle taken; the OS has no volume mounted on this device"
        )
        .map_err(io),
        Ok(volumes) => {
            writeln!(out, "write rehearsal: writable handle taken").map_err(io)?;
            for volume in &volumes {
                match (volume.locked, &volume.error) {
                    (true, _) => writeln!(out, "  volume {} locked", volume.volume),
                    (false, Some(err)) => {
                        writeln!(out, "  volume {} NOT locked - {err}", volume.volume)
                    }
                    (false, None) => writeln!(out, "  volume {} NOT locked", volume.volume),
                }
                .map_err(io)?;
            }
            Ok(())
        }
    }
}

/// Reads the partition table and nothing else.
fn pit(
    args: &PitArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let info = device.info().clone();
    let table = read_pit(&mut *device, opts.pit_at, trace)?;
    print_table(out, &table, opts.pit_at, &info)
}

fn dump(
    args: &DumpArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let range = resolve(&mut *device, &args.location, args.length, out, trace, opts)?
        .ok_or_else(|| Error::InvalidArgument("--offset or --partition is required".into()))?;
    let length = range
        .length
        .ok_or_else(|| Error::InvalidArgument("--length is required with --offset".into()))?;

    let output = longpath::for_open(&args.output);
    if opts.dry_run {
        return writeln!(
            out,
            "dry-run: would read {length} bytes from {} at {}..{} into {output:?}",
            args.device,
            range.offset,
            range.offset + length,
        )
        .map_err(|e| Error::io("writing output", e));
    }

    let mut file =
        File::create(&output).map_err(|e| Error::io(format!("creating {output:?}"), e))?;
    let mut bar = opts.bar("dump");
    let written = transfer::dump(
        &mut *device,
        range.offset,
        length,
        &mut file,
        trace,
        bar.as_mut(),
    )?;
    writeln!(
        out,
        "dumped {written} bytes from offset {} to {output:?}",
        range.offset
    )
    .map_err(|e| Error::io("writing output", e))
}

fn flash(
    args: &FlashArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::ReadWrite, trace)?;
    transfer::ensure_writable(device.info())?;

    let input = longpath::for_open(&args.input);
    let mut file = File::open(&input).map_err(|e| Error::io(format!("opening {input:?}"), e))?;
    let input_len = file
        .metadata()
        .map_err(|e| Error::io(format!("stat {input:?}"), e))?
        .len();

    let range = resolve(&mut *device, &args.location, None, out, trace, opts)?
        .ok_or_else(|| Error::InvalidArgument("--offset or --partition is required".into()))?;
    if let Some(limit) = range.length
        && input_len > limit
    {
        return Err(Error::InvalidArgument(format!(
            "input is {input_len} bytes but the target range is {limit} bytes"
        )));
    }

    if opts.dry_run {
        return writeln!(
            out,
            "dry-run: would write {input_len} bytes from {input:?} to {} at {}..{}",
            args.device,
            range.offset,
            range.offset + input_len,
        )
        .map_err(|e| Error::io("writing output", e));
    }

    let mut bar = opts.bar("flash");
    let written = transfer::flash(
        &mut *device,
        range.offset,
        input_len,
        &mut file,
        trace,
        bar.as_mut(),
    )?;
    match range.length {
        Some(capacity) => writeln!(
            out,
            "wrote {written} bytes to offset {} of {capacity} bytes available",
            range.offset
        ),
        None => writeln!(out, "wrote {written} bytes to offset {}", range.offset),
    }
    .map_err(|e| Error::io("writing output", e))?;

    if args.verify {
        compare(
            &mut *device,
            range.offset,
            written,
            &input,
            out,
            trace,
            opts,
        )?;
    }
    Ok(())
}

/// Reads the range back and compares it with the file it came from.
fn verify(
    args: &VerifyArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let input = longpath::for_open(&args.input);
    let length = std::fs::metadata(&input)
        .map_err(|e| Error::io(format!("stat {input:?}"), e))?
        .len();

    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let range = resolve(&mut *device, &args.location, None, out, trace, opts)?
        .ok_or_else(|| Error::InvalidArgument("--offset or --partition is required".into()))?;

    if opts.dry_run {
        return writeln!(
            out,
            "dry-run: would compare {length} bytes of {} at {}..{} against {input:?}",
            args.device,
            range.offset,
            range.offset + length,
        )
        .map_err(|e| Error::io("writing output", e));
    }

    compare(&mut *device, range.offset, length, &input, out, trace, opts)
}

fn compare(
    device: &mut dyn RawDevice,
    offset: u64,
    length: u64,
    source: &std::path::Path,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    const CHUNK: usize = 1 << 20;
    let mut bar = opts.bar("verify");
    let sector = device.info().logical_sector_size;
    let mut file = File::open(source).map_err(|e| Error::io(format!("opening {source:?}"), e))?;
    let mut from_device = vec![0u8; CHUNK];
    let mut from_file = vec![0u8; CHUNK];

    let mut done = 0u64;
    while done < length {
        let want = usize::try_from(length - done).unwrap_or(CHUNK).min(CHUNK);
        let aligned = usize::try_from(transfer::align_up(want as u64, sector)).unwrap_or(want);
        let at = offset + done;

        device
            .read_at(at, &mut from_device[..aligned])
            .map_err(|err| {
                trace.failed(format!("verify read {aligned}B at {at}"), &err);
                Error::Device(err)
            })?;
        file.read_exact(&mut from_file[..want])
            .map_err(|e| Error::io("reading input file", e))?;

        if let Some(i) = (0..want).position(|i| from_device[i] != from_file[i]) {
            return Err(Error::VerifyFailed {
                offset: at + i as u64,
                expected: from_file[i],
                found: from_device[i],
            });
        }
        done += want as u64;
        bar.advance(done, length);
    }
    bar.finish(done);

    writeln!(out, "verified {done} bytes at offset {offset}")
        .map_err(|e| Error::io("writing output", e))
}

/// The PIT is read only when `--partition` was given, and the range it resolves
/// to is printed, and checked against the device, before it is used.
fn resolve(
    device: &mut dyn RawDevice,
    location: &Location,
    explicit_length: Option<u64>,
    out: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<Option<Range>> {
    let selector = match (&location.partition, location.partition_id) {
        (Some(name), _) => Selector::Name(name),
        (None, Some(id)) => Selector::Id(id),
        (None, None) => {
            return Ok(location.offset.map(|offset| Range {
                offset,
                length: explicit_length,
            }));
        }
    };

    let info = device.info().clone();
    let table = read_pit(device, opts.pit_at, trace)?;
    let partition = match selector {
        Selector::Name(name) => table.find(name)?,
        Selector::Id(id) => table.find_by_id(id)?,
    };
    let start = partition.byte_offset();
    let end = start.saturating_add(partition.byte_length());

    writeln!(
        out,
        "pit: {} spans {start}..{end} ({} bytes) from block_offset={} block_count={} \
         device_type={} with block size {ASSUMED_BLOCK_SIZE} assumed",
        partition.name,
        partition.byte_length(),
        partition.block_offset,
        partition.block_count,
        partition.device_type,
    )
    .map_err(|e| Error::io("writing output", e))?;

    // A range past the end is the loudest signal that the layout was misread.
    if let Some(size) = info.size_bytes
        && end > size
    {
        return Err(Error::InvalidArgument(format!(
            "partition {} resolves to {start}..{end}, past the end of {} at {size} bytes; \
             the block size assumption or the table itself is wrong",
            partition.name, info.id,
        )));
    }

    let length = explicit_length.unwrap_or_else(|| partition.byte_length());
    if length > partition.byte_length() {
        return Err(Error::InvalidArgument(format!(
            "--length {length} is larger than partition {} at {} bytes",
            partition.name,
            partition.byte_length(),
        )));
    }

    Ok(Some(Range {
        offset: start,
        length: Some(length),
    }))
}

fn print_table(out: &mut dyn Write, table: &Pit, at: u64, info: &DeviceInfo) -> Result<()> {
    let io = |e| Error::io("writing output", e);

    writeln!(
        out,
        "pit: read at offset {at} - chip={:?} port={:?} format={:?}, {} entries",
        table.chip,
        table.port,
        table.format,
        table.partitions.len(),
    )
    .map_err(io)?;
    writeln!(
        out,
        "pit: block size {ASSUMED_BLOCK_SIZE} assumed; every byte column below depends on it"
    )
    .map_err(io)?;
    writeln!(out, "device: {}", describe(info)).map_err(io)?;
    writeln!(out).map_err(io)?;
    writeln!(
        out,
        "  {:<16} {:<7} {:>3}  {:>11} {:>9}  {:>14} {:>12} {:>10}  FLASH FILE",
        "NAME", "TYPE", "ID", "BLOCK OFF", "BLOCKS", "BYTE OFFSET", "BYTE LEN", "SIZE",
    )
    .map_err(io)?;

    for part in &table.partitions {
        let end = part.byte_offset().saturating_add(part.byte_length());
        let beyond = info.size_bytes.is_some_and(|size| end > size);
        writeln!(
            out,
            "  {:<16} {:<7} {:>3}  {:>11} {:>9}  {:>14} {:>12} {:>10}  {}{}",
            part.name,
            part.device_type.to_string(),
            part.identifier,
            part.block_offset,
            part.block_count,
            part.byte_offset(),
            part.byte_length(),
            human_size(part.byte_length()),
            if part.flash_filename.is_empty() {
                "-"
            } else {
                part.flash_filename.as_str()
            },
            if beyond { "  << beyond device end" } else { "" },
        )
        .map_err(io)?;
    }
    Ok(())
}

fn read_pit(device: &mut dyn RawDevice, at: u64, trace: &Trace) -> Result<Pit> {
    let sector = usize::try_from(device.info().logical_sector_size).unwrap_or(512);
    let mut head = vec![0u8; sector.max(rawio_core::pit::HEADER_LEN)];
    device.read_at(at, &mut head).map_err(|err| {
        trace.failed(format!("read PIT header at {at}"), &err);
        Error::Device(err)
    })?;
    trace.ok(Stage::ParsePit, format!("read PIT header at {at}"), "ok");

    let entry_count = u32::from_le_bytes(head[4..8].try_into().expect("header is long enough"));
    let needed = rawio_core::pit::HEADER_LEN + entry_count as usize * rawio_core::pit::ENTRY_LEN;
    if needed > head.len() {
        let aligned = usize::try_from(transfer::align_up(
            needed as u64,
            device.info().logical_sector_size,
        ))
        .map_err(|_| Error::Pit(format!("{entry_count} entries is implausible")))?;
        head.resize(aligned, 0);
        device.read_at(at, &mut head).map_err(|err| {
            trace.failed(format!("read PIT table at {at}"), &err);
            Error::Device(err)
        })?;
    }
    Pit::parse(&head).map_err(|err| match err {
        Error::Pit(message) => Error::Pit(format!(
            "{message} (looked at offset {at}; pass --pit-offset if the table is elsewhere)"
        )),
        other => other,
    })
}

fn describe(info: &DeviceInfo) -> String {
    let size = match info.size_bytes {
        Some(bytes) => format!("{:.1} GiB", bytes as f64 / (1u64 << 30) as f64),
        None => "unknown size".to_string(),
    };
    format!(
        "{}  {}  {}  sector={}  {}",
        info.id,
        size,
        info.removability.as_str(),
        info.logical_sector_size,
        info.description
    )
}
