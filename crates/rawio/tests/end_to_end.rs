//! Full command paths driven by an in-memory backend. This is where the
//! automated coverage stops: it proves the argument, range and reporting logic,
//! not that any OS agrees.

use std::cell::RefCell;
use std::rc::Rc;

use clap::Parser;
use rawio::app;
use rawio::cli::Cli;
use rawio_core::device::{Access, Backend, DeviceInfo, MemoryDevice, RawDevice, Removability};
use rawio_core::error::{DeviceError, Error, Stage};
use rawio_core::trace::Trace;

struct FakeBackend {
    device: Rc<RefCell<MemoryDevice>>,
}

impl FakeBackend {
    fn new(size: usize, removability: Removability) -> Self {
        Self {
            device: Rc::new(RefCell::new(MemoryDevice::new("mem0", size, removability))),
        }
    }
}

impl Backend for FakeBackend {
    fn enumerate(&self, _trace: &Trace) -> Result<Vec<DeviceInfo>, DeviceError> {
        Ok(vec![self.device.borrow().info().clone()])
    }

    fn open(
        &self,
        id: &str,
        _access: Access,
        _trace: &Trace,
    ) -> Result<Box<dyn RawDevice>, DeviceError> {
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
    let cli = Cli::parse_from(args);
    let mut out = Vec::new();
    let result = app::run(&cli, backend, &mut out, &Trace::new());
    (result, String::from_utf8(out).expect("output is UTF-8"))
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
    let (result, out) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--partition",
            "LOG",
            "-o",
            output.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(result.is_ok(), "{result:?}");
    assert!(
        out.contains("pit: LOG spans 8192..12288 (4096 bytes)"),
        "{out}"
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

    let (missing, _) = run(&["rawio", "pit", "mem0"], &backend);
    assert!(matches!(missing, Err(Error::Pit(_))), "{missing:?}");

    let (found, out) = run(&["rawio", "pit", "mem0", "--pit-offset", "4096"], &backend);
    assert!(found.is_ok(), "{found:?}");
    assert!(out.contains("LOG"), "{out}");
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
            "--partition",
            "LOG",
            "-o",
            by_name.to_str().unwrap(),
        ],
        &backend,
    );
    let by_id = dir.join("id.bin");
    let (identified, out) = run(
        &[
            "rawio",
            "dump",
            "mem0",
            "--partition-id",
            "1",
            "-o",
            by_id.to_str().unwrap(),
        ],
        &backend,
    );

    assert!(named.is_ok(), "{named:?}");
    assert!(identified.is_ok(), "{identified:?}");
    assert!(out.contains("pit: LOG spans 32768..98304"), "{out}");
    assert_eq!(
        std::fs::read(&by_name).unwrap(),
        std::fs::read(&by_id).unwrap()
    );
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

fn write_pit(backend: &FakeBackend, at: usize, entries: &[(&str, u32, u32)]) {
    let image = pit_image(entries);
    backend.device.borrow_mut().contents_mut()[at..at + image.len()].copy_from_slice(&image);
}

fn tempdir(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("rawio-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    base
}
