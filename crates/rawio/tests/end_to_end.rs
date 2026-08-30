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
    fn enumerate(&self) -> Result<Vec<DeviceInfo>, DeviceError> {
        Ok(vec![self.device.borrow().info().clone()])
    }

    fn open(&self, id: &str, _access: Access) -> Result<Box<dyn RawDevice>, DeviceError> {
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
    assert!(out.contains("pit: LOG -> offset=8192 length=4096"), "{out}");
    assert_eq!(std::fs::read(&output).unwrap().len(), 4096);
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

fn tempdir(tag: &str) -> std::path::PathBuf {
    let base = std::env::temp_dir().join(format!("rawio-test-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    base
}
