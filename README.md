# rawio

Raw offset read/write CLI for removable block devices (SD cards).

One command per operation, no GUI, no interactive prompts — scriptable on both
Windows 11 and Linux from the same argument syntax.

## Commands

```
rawio list                                  # enumerate candidate devices
rawio probe  <device>                       # non-destructive pre-flight report
rawio dump   <device> --offset N --length N --output FILE
rawio flash  <device> --offset N --input FILE
```

`--offset` / `--length` accept decimal, `0x` hex, and `K`/`M`/`G` suffixes. An
offset that is not a multiple of the device's logical sector size is rejected
rather than rounded. A write whose length is not a multiple of the sector size
reads the final sector back first, so the bytes after the image survive.

Target selection by PIT partition name is opt-in (`--partition NAME`) and always
prints the resolved offset and length before touching the device.

## Safety

`flash` refuses any device that is not reported as removable. There is no
override flag. This blocks fixed disks; it does **not** distinguish a USB SD card
reader from a USB external SSD — verify the target with `list` / `probe` first.

## Build

Two crates: `rawio-core` holds the device and transfer logic, `rawio` is the
command-line front end.

```
cargo build --release                                       # host
cargo build --release --target x86_64-pc-windows-gnu        # cross to Windows
cargo build --release --target x86_64-unknown-linux-musl    # cross to Linux, static
```

Cross linkers come from `brew install mingw-w64 musl-cross` on macOS; the
linker names are set in `.cargo/config.toml`. The Linux build is static, so it
runs on any x86_64 machine without a matching glibc.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 2 | invalid arguments |
| 3 | device access failure (see stage + OS error in the output) |
| 4 | refused: target is not removable |
| 5 | write aborted; last successfully written offset is reported |
| 6 | unsupported on this platform |
