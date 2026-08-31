//! Full command paths driven by an in-memory backend. This is where the
//! automated coverage stops: it proves the argument, range and reporting logic,
//! not that any OS agrees.

use std::cell::RefCell;
use std::rc::Rc;

use clap::Parser;
use rawio::app;
use rawio::cli::Cli;
use rawio_core::device::{
    Access, Backend, DeviceInfo, MemoryDevice, RawDevice, Removability, VolumeLock,
};
use rawio_core::error::{DeviceError, Error, Stage};
use rawio_core::trace::Trace;

struct FakeBackend {
    device: Rc<RefCell<MemoryDevice>>,
    volumes: Vec<VolumeLock>,
    /// Access requested by every `open`, in order.
    opens: RefCell<Vec<Access>>,
}

impl FakeBackend {
    fn new(size: usize, removability: Removability) -> Self {
        Self {
            device: Rc::new(RefCell::new(MemoryDevice::new("mem0", size, removability))),
            volumes: Vec::new(),
            opens: RefCell::new(Vec::new()),
        }
    }
}

impl Backend for FakeBackend {
    fn enumerate(&self, _trace: &Trace) -> Result<Vec<DeviceInfo>, DeviceError> {
        Ok(vec![self.device.borrow().info().clone()])
    }

    fn rehearse_write(&self, _id: &str, _trace: &Trace) -> Result<Vec<VolumeLock>, DeviceError> {
        Ok(self.volumes.clone())
    }

    fn open(
        &self,
        id: &str,
        access: Access,
        _trace: &Trace,
    ) -> Result<Box<dyn RawDevice>, DeviceError> {
        self.opens.borrow_mut().push(access);
        if id != "mem0" {
            return Err(DeviceError::new(
                Stage::Open,
                format!("no such device {id:?}"),
            ));
        }
        let info = self.device.borrow().info().clone();
        Ok(Box::new(Handle {
            info,
            inner: Rc::clone(&self.device),
        }))
    }
}

struct Handle {
    info: DeviceInfo,
    inner: Rc<RefCell<MemoryDevice>>,
}

impl RawDevice for Handle {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<usize, DeviceError> {
        self.inner.borrow_mut().read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<usize, DeviceError> {
        self.inner.borrow_mut().write_at(offset, buf)
    }

    fn flush(&mut self) -> Result<(), DeviceError> {
        self.inner.borrow_mut().flush()
    }
}

fn run(args: &[&str], backend: &dyn Backend) -> (Result<(), Error>, String) {
    let streams = run_streams(args, backend);
    (streams.result, streams.out)
}

/// The two streams kept apart, for the tests that care which one a line took.
struct Streams {
    result: Result<(), Error>,
    out: String,
    diag: String,
}

fn run_streams(args: &[&str], backend: &dyn Backend) -> Streams {
    let cli = Cli::parse_from(args);
    let mut out = Vec::new();
    let mut diag = Vec::new();
    let result = app::run(&cli, backend, &mut out, &mut diag, &Trace::new());
    Streams {
        result,
        out: String::from_utf8(out).expect("output is UTF-8"),
        diag: String::from_utf8(diag).expect("diagnostics are UTF-8"),
    }
}

fn pit_image(entries: &[(&str, u32, u32)]) -> Vec<u8> {
    let mut buf = vec![0u8; 28 + entries.len() * 132];
    buf[0..4].copy_from_slice(&0x1234_9876u32.to_le_bytes());
    buf[4..8].copy_from_slice(&(entries.len() as u32).to_le_bytes());
    buf[24..28].copy_from_slice(&1u32.to_le_bytes());
    for (i, (name, offset, count)) in entries.iter().enumerate() {
        let entry = &mut buf[28 + i * 132..][..132];
        entry[4..8].copy_from_slice(&2u32.to_le_bytes()); // device type: mmc
        entry[8..12].copy_from_slice(&(i as u32).to_le_bytes()); // identifier
        entry[20..24].copy_from_slice(&offset.to_le_bytes());
        entry[24..28].copy_from_slice(&count.to_le_bytes());
        entry[36..36 + name.len()].copy_from_slice(name.as_bytes());
    }
    buf
}

#[test]
fn list_reports_the_device_identifier_usable_as_an_argument() {
    let backend = FakeBackend::new(4096, Removability::Removable);
    let (result, out) = run(&["rawio", "list"], &backend);

    assert!(result.is_ok());
    assert!(out.starts_with("mem0 "), "{out}");
}

#[test]
fn flash_then_dump_round_trips_through_the_cli() {
    let dir = tempdir("roundtrip");
    let input = dir.join("image.bin");
    let output = dir.join("readback.bin");
    let data: Vec<u8> = (0..2048).map(|i| (i % 251) as u8).collect();
    std::fs::write(&input, &data).unwrap();

    let backend = FakeBackend::new(8192, Removability::Removable);
    let (flashed, _) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--offset",
            "1K",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );
    assert!(flashed.is_ok(), "{flashed:?}");

    let (dumped, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--offset",
            "1K",
            "--length",
            "2K",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );
    assert!(dumped.is_ok(), "{dumped:?}");
    assert_eq!(std::fs::read(&output).unwrap(), data);
}

#[test]
fn flash_to_a_fixed_device_exits_four_and_writes_nothing() {
    let dir = tempdir("fixed");
    let input = dir.join("image.bin");
    std::fs::write(&input, vec![0xFFu8; 512]).unwrap();

    let backend = FakeBackend::new(4096, Removability::Fixed);
    let (result, _) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--offset",
            "0",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );

    let err = result.unwrap_err();
    assert!(matches!(err, Error::NotRemovable { .. }), "{err}");
    assert_eq!(err.exit_code(), 4);
    assert!(backend.device.borrow().contents().iter().all(|b| *b == 0));
}

#[test]
fn partition_lookup_prints_the_resolved_range_before_use() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    backend.device.borrow_mut().contents_mut()[..28 + 132]
        .copy_from_slice(&pit_image(&[("LOG", 16, 8)]));

    let dir = tempdir("pit");
    let output = dir.join("log.bin");
    let run = run_streams(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(
        run.diag.contains("pit: LOG spans 8192..12288 (4096 bytes)"),
        "{}",
        run.diag
    );
    assert_eq!(std::fs::read(&output).unwrap().len(), 4096);
}

#[test]
fn the_pit_command_prints_every_entry() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("BOOT", 16, 8), ("LOG", 64, 128)]);

    let (result, out) = run(&["rawio", "pit", "mem0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("BOOT"), "{out}");
    assert!(out.contains("LOG"), "{out}");
    // Byte values next to the raw block values they were derived from.
    assert!(out.contains("32768"), "{out}");
    assert!(out.contains("65536"), "{out}");
}

#[test]
fn the_pit_can_be_read_from_somewhere_other_than_zero() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 4096, &[("LOG", 64, 128)]);

    let (found, out) = run(&["rawio", "pit", "mem0", "--pit-offset", "4096"], &backend);
    assert!(found.is_ok(), "{found:?}");
    assert!(out.contains("LOG"), "{out}");

    let (wrong, _) = run(&["rawio", "pit", "mem0", "--pit-offset", "8192"], &backend);
    assert!(matches!(wrong, Err(Error::Pit(_))), "{wrong:?}");
}

/// The card in hand: an MBR, and the PIT tucked into the space in front of the
/// first partition where nothing points at it.
#[test]
fn a_pit_in_front_of_the_first_partition_is_found_without_an_offset() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 4096)]);
    write_pit(&backend, 2047 * 512, &[("LOG", 64, 128)]);

    let run = run_streams(&["rawio", "pit", "mem0"], &backend);

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(
        run.diag.contains("pit: found at offset 1048064"),
        "{}",
        run.diag
    );
    assert!(run.out.contains("LOG"), "{}", run.out);
}

/// Two copies in the same gap: the one the partition was written against is
/// the one a backwards search reaches first.
#[test]
fn the_copy_nearest_the_first_partition_is_the_one_reported() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 4096)]);
    write_pit(&backend, 512, &[("OLD", 64, 128)]);
    write_pit(&backend, 2047 * 512, &[("LOG", 64, 128)]);

    let (result, out) = run(&["rawio", "pit", "mem0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("LOG") && !out.contains("OLD"), "{out}");
}

/// The search reads unallocated space, and how much of it is bounded.
#[test]
fn the_search_stops_at_the_budget_and_says_how_to_carry_on() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 4096)]);
    write_pit(&backend, 12 << 20, &[("LOG", 64, 128)]);

    let (stopped, _) = run(&["rawio", "pit", "mem0", "--pit-scan", "4K"], &backend);
    let message = stopped.unwrap_err().to_string();
    assert!(message.contains("--pit-scan"), "{message}");

    let (found, out) = run(&["rawio", "pit", "mem0", "--pit-scan", "0"], &backend);
    assert!(found.is_ok(), "{found:?}");
    assert!(out.contains("LOG"), "{out}");
}

#[test]
fn the_parts_command_prints_the_mbr_and_the_space_it_leaves() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 4096), (0x83, 8192, 4096)]);

    let (result, out) = run(&["rawio", "parts", "mem0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("scheme=mbr"), "{out}");
    assert!(out.contains("0x0c"), "{out}");
    // 2048 and 8192 sectors in, 4096 sectors long.
    assert!(out.contains("1048576") && out.contains("4194304"), "{out}");
    assert!(out.contains("unallocated 0..1048576"), "{out}");
    assert!(out.contains("backwards"), "{out}");
}

/// MBR entries have no names, so the index is the only selector, and a range
/// resolved from one is printed before it is read.
#[test]
fn a_range_resolves_from_an_mbr_entry_by_index() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);
    let output = tempdir("mbr").join("p1.bin");

    let run = run_streams(
        &[
            "rawio",
            "dump",
            "mem0",
            "--partition-id",
            "1",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(run.result.is_ok(), "{:?} {}", run.result, run.diag);
    assert!(run.diag.contains("parts: mbr #1"), "{}", run.diag);
    assert!(run.diag.contains("spans 1048576..1052672"), "{}", run.diag);
    assert_eq!(std::fs::read(&output).unwrap().len(), 4096);
}

#[test]
fn an_mbr_name_lookup_says_which_selector_works() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);
    let output = tempdir("mbrname").join("p1.bin");

    let (result, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--partition",
            "BOOT",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    let message = result.unwrap_err().to_string();
    assert!(message.contains("--partition-id"), "{message}");
    assert!(!output.exists());
}

/// A card carrying both is ambiguous, and guessing is what costs a card.
#[test]
fn a_hybrid_layout_is_refused_until_the_scheme_is_named() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 4096)]);
    backend.device.borrow_mut().contents_mut()[512..520].copy_from_slice(b"EFI PART");

    let (result, _) = run(&["rawio", "parts", "mem0"], &backend);

    let message = result.unwrap_err().to_string();
    assert!(message.contains("--scheme"), "{message}");

    let (named, out) = run(&["rawio", "parts", "mem0", "--scheme", "mbr"], &backend);
    assert!(named.is_ok(), "{named:?}");
    assert!(out.contains("scheme=mbr"), "{out}");
}

#[test]
fn an_entry_past_the_end_of_the_device_is_flagged() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("SANE", 16, 8), ("BAD", 99_999_999, 1024)]);

    let (result, out) = run(&["rawio", "pit", "mem0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("beyond device"), "{out}");
}

#[test]
fn a_partition_past_the_end_of_the_device_aborts_before_any_io() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("BAD", 99_999_999, 1024)]);
    let output = tempdir("beyond").join("bad.bin");

    let (result, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "BAD",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
    assert!(!output.exists());
}

#[test]
fn a_partition_id_selects_the_same_range_as_its_name() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("BOOT", 16, 8), ("LOG", 64, 128)]);
    let dir = tempdir("byid");

    let by_name = dir.join("name.bin");
    let (named, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-o",
            by_name.to_str().unwrap(),
        ],
        &backend,
    );
    let by_id = dir.join("id.bin");
    let identified = run_streams(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition-id",
            "1",
            "-o",
            by_id.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(named.is_ok(), "{named:?}");
    assert!(identified.result.is_ok(), "{:?}", identified.result);
    assert!(
        identified.diag.contains("pit: LOG spans 32768..98304"),
        "{}",
        identified.diag
    );
    assert_eq!(
        std::fs::read(&by_name).unwrap(),
        std::fs::read(&by_id).unwrap()
    );
}

#[test]
fn probe_rehearses_the_volume_locks_a_write_would_need() {
    let mut backend = FakeBackend::new(1 << 20, Removability::Removable);
    backend.volumes = vec![
        VolumeLock {
            volume: "E:".into(),
            locked: true,
            error: None,
        },
        VolumeLock {
            volume: "F:".into(),
            locked: false,
            error: Some(DeviceError::with_os_error(
                Stage::LockVolume,
                "access denied",
                5,
            )),
        },
    ];

    let (result, out) = run(&["rawio", "probe", "mem0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("E:") && out.contains("F:"), "{out}");
    assert!(out.contains("os error 5"), "{out}");
    // The card is untouched either way.
    assert!(backend.device.borrow().contents().iter().all(|b| *b == 0));
}

#[test]
fn probe_does_not_rehearse_a_write_it_would_refuse() {
    let mut backend = FakeBackend::new(1 << 20, Removability::Fixed);
    backend.volumes = vec![VolumeLock {
        volume: "C:".into(),
        locked: true,
        error: None,
    }];

    let (result, out) = run(&["rawio", "probe", "mem0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    assert!(!out.contains("C:"), "{out}");
    assert!(out.to_lowercase().contains("not removable"), "{out}");
}

#[test]
fn probe_reports_the_table_when_asked_for_it() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("LOG", 64, 128)]);

    let (without, quiet) = run(&["rawio", "probe", "mem0"], &backend);
    assert!(without.is_ok(), "{without:?}");
    assert!(!quiet.contains("LOG"), "{quiet}");

    let (with, loud) = run(&["rawio", "probe", "mem0", "--pit"], &backend);
    assert!(with.is_ok(), "{with:?}");
    assert!(loud.contains("LOG"), "{loud}");
}

#[test]
fn an_unknown_partition_name_lists_the_ones_that_exist() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("BOOT", 16, 8), ("LOG", 64, 128)]);
    let output = tempdir("unknown").join("x.bin");

    let (result, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "NOPE",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    let message = result.unwrap_err().to_string();
    assert!(
        message.contains("BOOT") && message.contains("LOG"),
        "{message}"
    );
}

#[test]
fn flashing_a_partition_reports_how_much_of_it_was_used() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("LOG", 64, 128)]);
    let input = tempdir("partial").join("small.bin");
    std::fs::write(&input, vec![0xEE; 1024]).unwrap();

    let (result, out) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("1024") && out.contains("65536"), "{out}");
}

#[test]
fn a_dry_run_dump_writes_no_file() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("LOG", 64, 128)]);
    let output = tempdir("dryread").join("log.bin");

    let (result, out) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-o",
            output.to_str().unwrap(),
            "--dry-run",
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("dry-run"), "{out}");
    assert!(!output.exists());
}

#[test]
fn a_dry_run_flash_leaves_the_device_untouched() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("LOG", 64, 128)]);
    let before = backend.device.borrow().contents().to_vec();
    let input = tempdir("drywrite").join("img.bin");
    std::fs::write(&input, vec![0x5A; 4096]).unwrap();

    let (result, out) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-i",
            input.to_str().unwrap(),
            "--dry-run",
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("dry-run"), "{out}");
    assert_eq!(backend.device.borrow().contents(), before.as_slice());
}

/// A dry run exists to show what the real run would do, so it has to run the
/// same range validation and refuse exactly what the real run refuses.
#[test]
fn a_dry_run_refuses_the_unaligned_offset_the_real_run_would() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let input = tempdir("dryunaligned").join("img.bin");
    std::fs::write(&input, vec![0u8; 512]).unwrap();

    let (result, _) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--offset",
            "100",
            "-i",
            input.to_str().unwrap(),
            "--dry-run",
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

#[test]
fn a_dry_run_refuses_a_range_that_overflows() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let output = tempdir("dryoverflow").join("x.bin");

    // Sector aligned, but offset + length wraps u64.
    let (result, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--offset",
            "18446744073709551104",
            "--length",
            "4096",
            "-o",
            output.to_str().unwrap(),
            "--dry-run",
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

#[test]
fn a_dry_run_refuses_a_range_past_the_device_end() {
    let backend = FakeBackend::new(8192, Removability::Removable);
    let input = tempdir("drybeyond").join("img.bin");
    std::fs::write(&input, vec![0u8; 8192]).unwrap();

    let (result, _) = run(
        &[
            "rawio",
            "verify",
            "mem0",
            "--offset",
            "4096",
            "-i",
            input.to_str().unwrap(),
            "--dry-run",
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

/// The Windows backend locks and dismounts mounted volumes on a writable open,
/// so a rehearsal must never ask for one; only the real flash may.
#[test]
fn a_dry_run_flash_never_opens_the_device_for_writing() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let input = tempdir("dryaccess").join("img.bin");
    std::fs::write(&input, vec![0x5A; 4096]).unwrap();
    let flash = |dry: bool| {
        let mut args = vec![
            "rawio",
            "flash",
            "mem0",
            "--offset",
            "0",
            "-i",
            input.to_str().unwrap(),
        ];
        if dry {
            args.push("--dry-run");
        }
        let (result, _) = run(&args, &backend);
        assert!(result.is_ok(), "dry={dry}: {result:?}");
    };

    flash(true);
    assert_eq!(*backend.opens.borrow(), vec![Access::Read]);

    flash(false);
    assert_eq!(
        *backend.opens.borrow(),
        vec![Access::Read, Access::ReadWrite]
    );
}

#[test]
fn flash_can_read_back_what_it_wrote() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let input = tempdir("verifyok").join("img.bin");
    std::fs::write(
        &input,
        (0..4096).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
    )
    .unwrap();

    let (result, out) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--offset",
            "4096",
            "-i",
            input.to_str().unwrap(),
            "--verify",
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("verified"), "{out}");
}

#[test]
fn verify_names_the_first_byte_that_differs() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let input = tempdir("verifybad").join("img.bin");
    std::fs::write(&input, vec![0x11; 4096]).unwrap();

    let (flashed, _) = run(
        &[
            "rawio",
            "flash",
            "mem0",
            "--offset",
            "4096",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );
    assert!(flashed.is_ok(), "{flashed:?}");
    backend.device.borrow_mut().contents_mut()[4096 + 100] = 0x22;

    let (result, _) = run(
        &[
            "rawio",
            "verify",
            "mem0",
            "--offset",
            "4096",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );

    let message = result.unwrap_err().to_string();
    assert!(message.contains("4196"), "{message}");
}

/// A file longer than the partition would be compared into the neighbouring
/// partition; flash refuses that input, so verify has to as well.
#[test]
fn verify_rejects_input_longer_than_the_partition() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("LOG", 64, 128)]);
    let input = tempdir("verifytoolong").join("big.bin");
    std::fs::write(&input, vec![0u8; 128 << 10]).unwrap();

    let (result, _) = run(
        &[
            "rawio",
            "verify",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

/// Verify walks the same device ranges the transfers do, so it has to refuse
/// exactly what they refuse instead of failing later with a raw device error.
#[test]
fn verify_rejects_an_unaligned_offset_like_dump_does() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let input = tempdir("verifyunaligned").join("img.bin");
    std::fs::write(&input, vec![0u8; 512]).unwrap();

    let (result, _) = run(
        &[
            "rawio",
            "verify",
            "mem0",
            "--offset",
            "100",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

#[test]
fn verify_rejects_a_range_past_the_device_end_before_reading() {
    let backend = FakeBackend::new(8192, Removability::Removable);
    let input = tempdir("verifybeyond").join("img.bin");
    std::fs::write(&input, vec![0u8; 8192]).unwrap();

    let (result, _) = run(
        &[
            "rawio",
            "verify",
            "mem0",
            "--offset",
            "4096",
            "-i",
            input.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

#[test]
fn a_length_larger_than_the_partition_is_rejected() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    write_pit(&backend, 0, &[("LOG", 64, 128)]);
    let output = tempdir("toolong").join("log.bin");

    let (result, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "--length",
            "128K",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
    assert!(!output.exists());
}

#[test]
fn a_missing_table_says_where_it_looked() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);

    let (result, _) = run(&["rawio", "pit", "mem0"], &backend);

    let message = result.unwrap_err().to_string();
    assert!(message.contains("--pit-offset"), "{message}");
}

/// Garbage at the PIT offset carries a garbage entry count; the parse failure
/// has to come from the magic check, never from trying to size a read by it.
#[test]
fn garbage_where_the_pit_should_be_fails_on_the_magic() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    {
        let mut device = backend.device.borrow_mut();
        let sector = &mut device.contents_mut()[..512];
        sector.fill(0xA5); // wrong magic
        sector[4..8].copy_from_slice(&100_000u32.to_le_bytes()); // absurd count
    }

    let (result, _) = run(&["rawio", "pit", "mem0", "--pit-offset", "0"], &backend);

    let message = result.unwrap_err().to_string();
    assert!(message.contains("magic"), "{message}");

    // Without an offset the same garbage is simply never a candidate.
    let (searched, _) = run(&["rawio", "pit", "mem0"], &backend);
    assert!(searched.unwrap_err().to_string().contains("no PIT found"));
}

#[test]
fn a_device_without_a_pit_aborts_instead_of_transferring() {
    let backend = FakeBackend::new(1 << 20, Removability::Removable);
    let dir = tempdir("nopit");
    let output = dir.join("log.bin");

    let (result, _) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--scheme",
            "pit",
            "--partition",
            "LOG",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(matches!(result, Err(Error::Pit(_))), "{result:?}");
    assert!(!output.exists());
}

/// `(type, first LBA, sectors)` in the primary slots, in order.
fn write_mbr(backend: &FakeBackend, entries: &[(u8, u32, u32)]) {
    let mut device = backend.device.borrow_mut();
    let sector = &mut device.contents_mut()[..512];
    sector[510..512].copy_from_slice(&[0x55, 0xAA]);
    for (i, (kind, start, sectors)) in entries.iter().enumerate() {
        let entry = &mut sector[446 + i * 16..][..16];
        entry[4] = *kind;
        entry[8..12].copy_from_slice(&start.to_le_bytes());
        entry[12..16].copy_from_slice(&sectors.to_le_bytes());
    }
}

fn write_pit(backend: &FakeBackend, at: usize, entries: &[(&str, u32, u32)]) {
    let image = pit_image(entries);
    backend.device.borrow_mut().contents_mut()[at..at + image.len()].copy_from_slice(&image);
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("rawio-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// The dump is what `hexdump -C` prints of the same bytes, including the line
/// that says where it stopped.
#[test]
fn hex_prints_the_bytes_at_the_offset_it_was_given() {
    let backend = FakeBackend::new(4096, Removability::Removable);
    backend.device.borrow_mut().contents_mut()[..16]
        .copy_from_slice(b"\xeb\x3c\x90MSDOS5.0\x00\x02\x08\x20\x00");

    let (result, out) = run(
        &["rawio", "hex", "mem0", "--offset", "0", "--length", "16"],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(
        out,
        "00000000  eb 3c 90 4d 53 44 4f 53  35 2e 30 00 02 08 20 00  |.<.MSDOS5.0... .|\n\
         00000010\n"
    );
}

/// A structure does not begin where its sector does, and looking at one is the
/// whole point of the command.
#[test]
fn hex_starts_at_an_offset_no_sector_starts_at() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);

    let (result, out) = run(
        &[
            "rawio", "hex", "mem0", "--offset", "0x1be", "--length", "16",
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(
        out.starts_with("000001be  00 00 00 00 0c 00 00 00"),
        "{out}"
    );
    assert!(out.trim_end().ends_with("000001ce"), "{out}");
}

/// The partition forms resolve exactly as they do for a transfer, and say what
/// they resolved to before a byte is printed.
#[test]
fn hex_reads_the_partition_the_table_names() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);
    backend.device.borrow_mut().contents_mut()[1 << 20..(1 << 20) + 4].copy_from_slice(b"BOOT");

    let run = run_streams(
        &[
            "rawio",
            "hex",
            "mem0",
            "--partition-id",
            "1",
            "--length",
            "16",
        ],
        &backend,
    );

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(run.diag.contains("parts: mbr #1"), "{}", run.diag);
    assert!(run.out.contains("00100000  42 4f 4f 54"), "{}", run.out);
}

/// Most of a card is one repeated line, and printing all of them hides the one
/// that matters. The default length is one sector.
#[test]
fn hex_collapses_the_runs_a_card_is_mostly_made_of() {
    let backend = FakeBackend::new(4096, Removability::Removable);

    let (result, out) = run(&["rawio", "hex", "mem0", "--offset", "0"], &backend);

    assert!(result.is_ok(), "{result:?}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert_eq!(lines[1], "*");
    assert_eq!(lines[2], "00000200");
}

#[test]
fn hex_prints_every_line_when_the_squeeze_is_off() {
    let backend = FakeBackend::new(4096, Removability::Removable);

    let (result, out) = run(
        &[
            "rawio",
            "hex",
            "mem0",
            "--offset",
            "0",
            "--length",
            "64",
            "--no-squeeze",
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert_eq!(out.lines().count(), 5, "{out}");
    assert!(!out.contains('*'), "{out}");
}

/// A rehearsal reads nothing, which is the only way to check a range on a
/// device that must not be touched.
#[test]
fn a_dry_run_hex_reads_nothing() {
    let backend = FakeBackend::new(4096, Removability::Removable);
    backend.device.borrow_mut().fail_reads_from(0);

    let (result, out) = run(
        &[
            "rawio",
            "hex",
            "mem0",
            "--offset",
            "0",
            "--length",
            "32",
            "--dry-run",
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(out.contains("dry-run: would read 32 bytes"), "{out}");
    assert!(!out.contains('|'), "{out}");
}

#[test]
fn hex_refuses_a_range_past_the_device_end() {
    let backend = FakeBackend::new(4096, Removability::Removable);

    let (result, _) = run(
        &[
            "rawio", "hex", "mem0", "--offset", "4000", "--length", "512",
        ],
        &backend,
    );

    assert!(
        matches!(result, Err(Error::InvalidArgument(_))),
        "{result:?}"
    );
}

/// A script reads stdout, and `rawio hex` exists to diff against the
/// `hexdump -C` the reader already has. How the range was arrived at is for a
/// person, so it takes stderr and leaves the dump alone.
#[test]
fn a_partition_hexdump_puts_nothing_but_the_dump_on_stdout() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);

    let run = run_streams(&["rawio", "hex", "mem0", "--partition-id", "1"], &backend);

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(run.out.starts_with("00100000"), "{}", run.out);
    assert!(!run.out.contains("parts:"), "{}", run.out);
    assert!(run.diag.contains("parts: mbr #1"), "{}", run.diag);
}

/// The same rule for the PIT: a search prints where it looked and what it
/// found, and none of that belongs in what a script parses.
#[test]
fn a_pit_search_reports_itself_on_stderr() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 4096)]);
    write_pit(&backend, 2047 * 512, &[("LOG", 64, 128)]);

    let run = run_streams(&["rawio", "pit", "mem0"], &backend);

    assert!(run.result.is_ok(), "{:?}", run.result);
    assert!(!run.out.contains("pit: searching"), "{}", run.out);
    assert!(!run.out.contains("pit: found at offset"), "{}", run.out);
    assert!(run.diag.contains("pit: searching"), "{}", run.diag);
    assert!(run.diag.contains("pit: found at offset"), "{}", run.diag);
    assert!(run.out.contains("LOG"), "{}", run.out);
}

/// --pit-offset says where a PIT is. Which table a range comes from is what
/// --scheme says, and letting the offset quietly decide it changed what
/// `--partition-id 1` meant on a card that carries both.
#[test]
fn a_pit_offset_does_not_choose_the_table() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);
    write_pit(&backend, 4096, &[("LOG", 64, 128)]);

    let (refused, _) = run(
        &["rawio", "parts", "mem0", "--pit-offset", "4096"],
        &backend,
    );
    assert!(
        matches!(refused, Err(Error::InvalidArgument(_))),
        "{refused:?}"
    );

    // Said outright, the offset is honoured and the MBR is not consulted.
    let (asked, out) = run(
        &[
            "rawio",
            "parts",
            "mem0",
            "--scheme",
            "pit",
            "--pit-offset",
            "4096",
        ],
        &backend,
    );
    assert!(asked.is_ok(), "{asked:?}");
    assert!(out.contains("LOG"), "{out}");
}

/// `probe --pit` reads one, so the offset has something to say there without
/// a --scheme that would also redirect --partition.
#[test]
fn probe_takes_a_pit_offset_alongside_the_partition_table() {
    let backend = FakeBackend::new(16 << 20, Removability::Removable);
    write_mbr(&backend, &[(0x0c, 2048, 8)]);
    write_pit(&backend, 4096, &[("LOG", 64, 128)]);

    let run = run_streams(
        &[
            "rawio",
            "probe",
            "mem0",
            "--parts",
            "--pit",
            "--pit-offset",
            "4096",
            "--partition-id",
            "1",
        ],
        &backend,
    );

    assert!(run.result.is_ok(), "{:?} {}", run.result, run.diag);
    // The MBR entry, not the PIT entry that also answers to 1.
    assert!(run.diag.contains("parts: mbr #1"), "{}", run.diag);
    assert!(run.out.contains("resolved: offset=1048576"), "{}", run.out);
    assert!(run.out.contains("LOG"), "{}", run.out);
}
