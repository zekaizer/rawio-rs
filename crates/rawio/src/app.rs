//! Command dispatch. Takes the backend as an argument so the whole flow can be
//! driven by a fake device in tests.

use std::fs::File;
use std::io::Write;

use crate::cli::{Cli, Command, DumpArgs, FlashArgs, Location, ProbeArgs};
use rawio_core::device::{Access, Backend, DeviceInfo, RawDevice};
use rawio_core::error::{Error, Result, Stage};
use rawio_core::pit::Pit;
use rawio_core::trace::Trace;
use rawio_core::transfer;

/// Resolved target range. `length` is absent when only the caller knows it
/// (a `flash` whose length comes from the input file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub offset: u64,
    pub length: Option<u64>,
}

pub fn run(cli: &Cli, backend: &dyn Backend, out: &mut dyn Write, trace: &Trace) -> Result<()> {
    match &cli.command {
        Command::List => list(backend, out),
        Command::Probe(args) => probe(args, backend, out, trace),
        Command::Dump(args) => dump(args, backend, out, trace),
        Command::Flash(args) => flash(args, backend, out, trace),
    }
}

fn list(backend: &dyn Backend, out: &mut dyn Write) -> Result<()> {
    let devices = backend.enumerate()?;
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
) -> Result<()> {
    let devices = backend.enumerate()?;
    for info in &devices {
        writeln!(out, "device: {}", describe(info)).map_err(|e| Error::io("writing output", e))?;
    }

    let mut device = backend.open(&args.device, Access::Read)?;
    let info = device.info().clone();
    writeln!(out, "target: {}", describe(&info)).map_err(|e| Error::io("writing output", e))?;
    writeln!(out, "writable: {}", info.removability.writable())
        .map_err(|e| Error::io("writing output", e))?;

    if let Some(range) = resolve(&mut *device, &args.location, None, out, trace)? {
        writeln!(
            out,
            "resolved: offset={} length={:?}",
            range.offset, range.length
        )
        .map_err(|e| Error::io("writing output", e))?;
    }
    Ok(())
}

fn dump(args: &DumpArgs, backend: &dyn Backend, out: &mut dyn Write, trace: &Trace) -> Result<()> {
    let mut device = backend.open(&args.device, Access::Read)?;
    let range = resolve(&mut *device, &args.location, args.length, out, trace)?
        .ok_or_else(|| Error::InvalidArgument("--offset or --partition is required".into()))?;
    let length = range
        .length
        .ok_or_else(|| Error::InvalidArgument("--length is required with --offset".into()))?;

    let mut file = File::create(&args.output)
        .map_err(|e| Error::io(format!("creating {:?}", args.output), e))?;
    let written = transfer::dump(&mut *device, range.offset, length, &mut file, trace)?;
    writeln!(
        out,
        "dumped {written} bytes from offset {} to {:?}",
        range.offset, args.output
    )
    .map_err(|e| Error::io("writing output", e))
}

fn flash(
    args: &FlashArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    trace: &Trace,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::ReadWrite)?;
    transfer::ensure_writable(device.info())?;

    let mut file =
        File::open(&args.input).map_err(|e| Error::io(format!("opening {:?}", args.input), e))?;
    let input_len = file
        .metadata()
        .map_err(|e| Error::io(format!("stat {:?}", args.input), e))?
        .len();

    let range = resolve(&mut *device, &args.location, None, out, trace)?
        .ok_or_else(|| Error::InvalidArgument("--offset or --partition is required".into()))?;
    if let Some(limit) = range.length
        && input_len > limit
    {
        return Err(Error::InvalidArgument(format!(
            "input is {input_len} bytes but the target range is {limit} bytes"
        )));
    }

    let written = transfer::flash(&mut *device, range.offset, input_len, &mut file, trace)?;
    writeln!(out, "wrote {written} bytes to offset {}", range.offset)
        .map_err(|e| Error::io("writing output", e))
}

/// The PIT is read only when `--partition` was given, and the range it resolves
/// to is printed before it is used.
fn resolve(
    device: &mut dyn RawDevice,
    location: &Location,
    explicit_length: Option<u64>,
    out: &mut dyn Write,
    trace: &Trace,
) -> Result<Option<Range>> {
    if let Some(name) = &location.partition {
        let pit = read_pit(device, Location::DEFAULT_PIT_OFFSET, trace)?;
        let partition = pit.find(name)?;
        let range = Range {
            offset: partition.byte_offset(),
            length: Some(explicit_length.unwrap_or_else(|| partition.byte_length())),
        };
        writeln!(
            out,
            "pit: {} -> offset={} length={} (block_offset={} block_count={}, assumed block size {})",
            partition.name,
            range.offset,
            partition.byte_length(),
            partition.block_offset,
            partition.block_count,
            rawio_core::pit::ASSUMED_BLOCK_SIZE,
        )
        .map_err(|e| Error::io("writing output", e))?;
        return Ok(Some(range));
    }

    Ok(location.offset.map(|offset| Range {
        offset,
        length: explicit_length,
    }))
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
    Pit::parse(&head)
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
