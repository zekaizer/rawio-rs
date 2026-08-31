//! Command dispatch. Takes the backend as an argument so the whole flow can be
//! driven by a fake device in tests.

use std::fs::File;
use std::io::Write;

use crate::cli::{
    Cli, Command, DumpArgs, FlashArgs, HexArgs, Location, PartsArgs, PitArgs, PitSource, ProbeArgs,
    SchemeArg, TableSource, TransferOptions, VerifyArgs,
};
use crate::hexdump::Hexdump;
use crate::longpath;
use crate::progress::{Bar, human_size};
use rawio_core::device::{Access, Backend, DeviceInfo, RawDevice};
use rawio_core::error::{Error, Result};
use rawio_core::parts::{self, DeviceSectors, Gap, Scheme, Table};
use rawio_core::pit::{self, ASSUMED_BLOCK_SIZE, Found, Pit};
use rawio_core::progress::{Progress, Silent};
use rawio_core::trace::Trace;
use rawio_core::transfer;

/// Which table the range comes from, once the arguments have been read. Only
/// `--scheme` decides this: `--pit-offset` says where a PIT is, not that a
/// range should come from one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Detect,
    Table(Scheme),
    Pit,
}

/// Settings that apply to every command, lifted out of the parsed arguments so
/// the handlers do not each grow another parameter.
struct Options {
    source: Source,
    pit_at: Option<u64>,
    pit_scan: Option<u64>,
    dry_run: bool,
    progress: bool,
}

impl Options {
    /// A command that moves bytes: it can be rehearsed and it can report.
    fn transfer(table: &TableSource, transfer: &TransferOptions) -> Result<Self> {
        Ok(Self {
            dry_run: transfer.dry_run,
            progress: Bar::enabled(transfer.no_progress),
            ..Self::inspect(table, false)?
        })
    }

    /// A command that only looks: neither rehearsal nor progress applies.
    /// `reads_a_pit` is what `probe --pit` sets, being the one command that
    /// reads a PIT without resolving a range from it.
    fn inspect(table: &TableSource, reads_a_pit: bool) -> Result<Self> {
        let source = match table.scheme {
            SchemeArg::Mbr => Source::Table(Scheme::Mbr),
            SchemeArg::Gpt => Source::Table(Scheme::Gpt),
            SchemeArg::Pit => Source::Pit,
            SchemeArg::Auto => Source::Detect,
        };
        if table.pit_source.pit_offset.is_some() && source != Source::Pit && !reads_a_pit {
            return Err(Error::InvalidArgument(
                "--pit-offset says where a PIT is, not which table to use; \
                 add --scheme pit to resolve a range from one"
                    .into(),
            ));
        }
        Ok(Self {
            source,
            pit_at: table.pit_source.pit_offset,
            pit_scan: table.pit_source.pit_scan.bytes(),
            dry_run: false,
            progress: false,
        })
    }

    /// `rawio pit` fixes the scheme; only where to look is still an argument.
    fn pit(pit: &PitSource) -> Self {
        Self {
            source: Source::Pit,
            pit_at: pit.pit_offset,
            pit_scan: pit.pit_scan.bytes(),
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

/// `out` carries results, which a script reads. `diag` carries everything that
/// only explains one: how a range was arrived at, and where a PIT was looked for.
pub fn run(
    cli: &Cli,
    backend: &dyn Backend,
    out: &mut dyn Write,
    diag: &mut dyn Write,
    trace: &Trace,
) -> Result<()> {
    match &cli.command {
        Command::List => list(backend, out, trace),
        Command::Probe(args) => probe(
            args,
            backend,
            out,
            diag,
            trace,
            &Options::inspect(&args.table, args.pit)?,
        ),
        Command::Parts(args) => parts_cmd(
            args,
            backend,
            out,
            diag,
            trace,
            &Options::inspect(&args.table, false)?,
        ),
        Command::Pit(args) => pit_cmd(
            args,
            backend,
            out,
            diag,
            trace,
            &Options::pit(&args.pit_source),
        ),
        Command::Hex(args) => {
            let opts = Options {
                dry_run: args.dry_run,
                ..Options::inspect(&args.table, false)?
            };
            hex(args, backend, out, diag, trace, &opts)
        }
        Command::Dump(args) => {
            let opts = Options::transfer(&args.table, &args.transfer)?;
            dump(args, backend, out, diag, trace, &opts)
        }
        Command::Flash(args) => {
            let opts = Options::transfer(&args.table, &args.transfer)?;
            flash(args, backend, out, diag, trace, &opts)
        }
        Command::Verify(args) => {
            let opts = Options::transfer(&args.table, &args.transfer)?;
            verify(args, backend, out, diag, trace, &opts)
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
    diag: &mut dyn Write,
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

    if args.parts {
        let table = read_table(&mut *device, opts, trace)?;
        print_parts(out, &table, &info)?;
    }

    if args.pit {
        let found = locate_pit(&mut *device, diag, trace, opts)?;
        print_table(out, &found.pit, found.offset, &info)?;
    }

    if let Some(location) = args.location.given() {
        let range = resolve(&mut *device, location, None, diag, trace, opts)?;
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

/// Reads the table the device carries and nothing else.
fn parts_cmd(
    args: &PartsArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let info = device.info().clone();

    if opts.source == Source::Pit {
        let found = locate_pit(&mut *device, diag, trace, opts)?;
        return print_table(out, &found.pit, found.offset, &info);
    }

    let table = read_table(&mut *device, opts, trace)?;
    print_parts(out, &table, &info)
}

/// Reads the PIT and nothing else.
fn pit_cmd(
    args: &PitArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let info = device.info().clone();
    let found = locate_pit(&mut *device, diag, trace, opts)?;
    print_table(out, &found.pit, found.offset, &info)
}

/// Prints a range the way `hexdump -C` would. Nothing is written anywhere, so
/// this is the one way to look at a structure before deciding what to do to it.
fn hex(
    args: &HexArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let io = |e| Error::io("writing output", e);

    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let range = resolve(
        &mut *device,
        &args.location,
        Some(args.length),
        diag,
        trace,
        opts,
    )?;
    let length = range.length.unwrap_or(args.length);

    let end = transfer::check_read_range(device.info(), range.offset, length)?;
    if opts.dry_run {
        return writeln!(
            out,
            "dry-run: would read {length} bytes from {} at {}..{end} as a hexdump",
            args.device, range.offset,
        )
        .map_err(io);
    }

    let mut dump = Hexdump::new(range.offset, end, !args.no_squeeze);
    transfer::read_range(&mut *device, range.offset, length, trace, &mut |bytes| {
        dump.push(bytes, out).map_err(io)
    })?;
    dump.finish(out).map_err(io)
}

fn dump(
    args: &DumpArgs,
    backend: &dyn Backend,
    out: &mut dyn Write,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let range = resolve(&mut *device, &args.location, args.length, diag, trace, opts)?;
    let length = range
        .length
        .ok_or_else(|| Error::InvalidArgument("--length is required with --offset".into()))?;

    let output = longpath::for_open(&args.output);
    if opts.dry_run {
        let end = transfer::check_range(device.info(), range.offset, length)?;
        return writeln!(
            out,
            "dry-run: would read {length} bytes from {} at {}..{end} into {output:?}",
            args.device, range.offset,
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
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    // Opening for write is what locks and dismounts mounted volumes on
    // Windows, and a rehearsal must do neither.
    let access = if opts.dry_run {
        Access::Read
    } else {
        Access::ReadWrite
    };
    let mut device = backend.open(&args.device, access, trace)?;
    transfer::ensure_writable(device.info())?;

    let input = longpath::for_open(&args.input);
    let mut file = File::open(&input).map_err(|e| Error::io(format!("opening {input:?}"), e))?;
    let input_len = file
        .metadata()
        .map_err(|e| Error::io(format!("stat {input:?}"), e))?
        .len();

    let range = resolve(&mut *device, &args.location, None, diag, trace, opts)?;
    if let Some(limit) = range.length
        && input_len > limit
    {
        return Err(Error::InvalidArgument(format!(
            "input is {input_len} bytes but the target range is {limit} bytes"
        )));
    }

    if opts.dry_run {
        let end = transfer::check_range(device.info(), range.offset, input_len)?;
        return writeln!(
            out,
            "dry-run: would write {input_len} bytes from {input:?} to {} at {}..{end}",
            args.device, range.offset,
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
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<()> {
    let input = longpath::for_open(&args.input);
    let length = std::fs::metadata(&input)
        .map_err(|e| Error::io(format!("stat {input:?}"), e))?
        .len();

    let mut device = backend.open(&args.device, Access::Read, trace)?;
    let range = resolve(&mut *device, &args.location, None, diag, trace, opts)?;
    if let Some(limit) = range.length
        && length > limit
    {
        return Err(Error::InvalidArgument(format!(
            "input is {length} bytes but the target range is {limit} bytes"
        )));
    }

    if opts.dry_run {
        let end = transfer::check_range(device.info(), range.offset, length)?;
        return writeln!(
            out,
            "dry-run: would compare {length} bytes of {} at {}..{end} against {input:?}",
            args.device, range.offset,
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
    let mut file = File::open(source).map_err(|e| Error::io(format!("opening {source:?}"), e))?;
    let mut bar = opts.bar("verify");
    let done = transfer::verify(device, offset, length, &mut file, trace, bar.as_mut())?;

    writeln!(out, "verified {done} bytes at offset {offset}")
        .map_err(|e| Error::io("writing output", e))
}

/// A table is read only when `--partition` was given, and the range it resolves
/// to is printed, and checked against the device, before it is used.
fn resolve(
    device: &mut dyn RawDevice,
    location: &Location,
    explicit_length: Option<u64>,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<Range> {
    let selector = match (&location.partition, location.partition_id) {
        (Some(name), _) => Selector::Name(name),
        (None, Some(id)) => Selector::Id(id),
        // The location group is required and exclusive, so this is the offset form.
        (None, None) => {
            let offset = location
                .offset
                .expect("the location group admits nothing else");
            return Ok(Range {
                offset,
                length: explicit_length,
            });
        }
    };

    if opts.source != Source::Pit {
        return resolve_in_table(device, selector, explicit_length, diag, trace, opts);
    }

    let info = device.info().clone();
    let table = locate_pit(device, diag, trace, opts)?.pit;
    let partition = match selector {
        Selector::Name(name) => table.find(name)?,
        Selector::Id(id) => table.find_by_id(id)?,
    };
    let start = partition.byte_offset();
    let end = start.saturating_add(partition.byte_length());

    writeln!(
        diag,
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

    Ok(Range {
        offset: start,
        length: Some(length),
    })
}

/// Resolves a range from the MBR or GPT, printing what it resolved to and
/// refusing anything the device cannot hold.
fn resolve_in_table(
    device: &mut dyn RawDevice,
    selector: Selector<'_>,
    explicit_length: Option<u64>,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<Range> {
    let info = device.info().clone();
    let table = read_table(device, opts, trace)?;
    let partition = match selector {
        Selector::Name(name) => table.find(name)?,
        Selector::Id(index) => table.find_by_index(index)?,
    };

    writeln!(
        diag,
        "parts: {} #{} {} spans {}..{} ({} bytes), type {}, from {}",
        table.scheme,
        partition.index,
        partition.name.as_deref().unwrap_or("-"),
        partition.start,
        partition.end(),
        partition.length,
        partition.kind,
        table.source,
    )
    .map_err(|e| Error::io("writing output", e))?;

    if let Some(size) = info.size_bytes
        && partition.end() > size
    {
        return Err(Error::InvalidArgument(format!(
            "partition {} resolves to {}..{}, past the end of {} at {size} bytes; \
             the table is wrong",
            partition.index,
            partition.start,
            partition.end(),
            info.id,
        )));
    }

    let length = explicit_length.unwrap_or(partition.length);
    if length > partition.length {
        return Err(Error::InvalidArgument(format!(
            "--length {length} is larger than partition {} at {} bytes",
            partition.index, partition.length,
        )));
    }

    Ok(Range {
        offset: partition.start,
        length: Some(length),
    })
}

fn read_table(device: &mut dyn RawDevice, opts: &Options, trace: &Trace) -> Result<Table> {
    let scheme = match opts.source {
        Source::Table(scheme) => Some(scheme),
        _ => None,
    };
    parts::read(&mut DeviceSectors::new(device, trace), scheme)
}

/// Reads the PIT from where it was said to be, or finds it. A search prints
/// where it landed: nothing else in the output would show that it found the
/// wrong copy.
fn locate_pit(
    device: &mut dyn RawDevice,
    diag: &mut dyn Write,
    trace: &Trace,
    opts: &Options,
) -> Result<Found> {
    if let Some(at) = opts.pit_at {
        let hint = |err| match err {
            Error::Pit(message) => Error::Pit(format!(
                "{message} (looked at offset {at}; drop --pit-offset to search for the table)"
            )),
            other => other,
        };
        let pit = pit::read_at(&mut DeviceSectors::new(device, trace), at).map_err(hint)?;
        return Ok(Found { pit, offset: at });
    }

    let gaps = search_space(device, diag, trace)?;
    let found = pit::scan(&mut DeviceSectors::new(device, trace), &gaps, opts.pit_scan)?;
    writeln!(
        diag,
        "pit: found at offset {} by searching the space no partition covers",
        found.offset
    )
    .map_err(|e| Error::io("writing output", e))?;
    Ok(found)
}

/// Where a PIT can be. A device with no readable partition table has no gaps
/// to speak of, so the whole of it is the search space.
fn search_space(
    device: &mut dyn RawDevice,
    diag: &mut dyn Write,
    trace: &Trace,
) -> Result<Vec<Gap>> {
    let size = device.info().size_bytes;
    let table = parts::read(&mut DeviceSectors::new(device, trace), None);
    let gaps = match &table {
        Ok(table) => table.gaps(size),
        Err(err) => {
            writeln!(
                diag,
                "pit: no partition table to bound the search ({err}); searching from offset 0"
            )
            .map_err(|e| Error::io("writing output", e))?;
            vec![Gap {
                start: 0,
                end: size.unwrap_or(u64::MAX),
                reverse: false,
            }]
        }
    };

    let where_ = gaps
        .iter()
        .map(|gap| {
            format!(
                "{}..{}{}",
                gap.start,
                gap.end,
                if gap.reverse { " backwards" } else { "" }
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(diag, "pit: searching {where_}").map_err(|e| Error::io("writing output", e))?;
    Ok(gaps)
}

/// The scheme, where the entries came from, and every range they resolve to -
/// the whole point of the command is that this is printable without acting.
fn print_parts(out: &mut dyn Write, table: &Table, info: &DeviceInfo) -> Result<()> {
    let io = |e| Error::io("writing output", e);

    writeln!(
        out,
        "parts: scheme={}, from {}, {} entries",
        table.scheme,
        table.source,
        table.partitions.len()
    )
    .map_err(io)?;
    writeln!(out, "device: {}", describe(info)).map_err(io)?;
    writeln!(out).map_err(io)?;
    writeln!(
        out,
        "  {:>3}  {:<24} {:<38} {:>14} {:>14} {:>10}",
        "ID", "NAME", "TYPE", "START", "LENGTH", "SIZE",
    )
    .map_err(io)?;

    for part in &table.partitions {
        let beyond = info.size_bytes.is_some_and(|size| part.end() > size);
        writeln!(
            out,
            "  {:>3}  {:<24} {:<38} {:>14} {:>14} {:>10}{}",
            part.index,
            part.name.as_deref().unwrap_or("-"),
            part.kind,
            part.start,
            part.length,
            human_size(part.length),
            if beyond { "  << beyond device end" } else { "" },
        )
        .map_err(io)?;
    }

    let gaps = table.gaps(info.size_bytes);
    if !gaps.is_empty() {
        writeln!(out).map_err(io)?;
        for gap in &gaps {
            writeln!(
                out,
                "  unallocated {}..{} ({}){}",
                gap.start,
                gap.end,
                human_size(gap.len()),
                if gap.reverse {
                    "  << a PIT search looks here first, backwards"
                } else {
                    ""
                },
            )
            .map_err(io)?;
        }
    }
    Ok(())
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
