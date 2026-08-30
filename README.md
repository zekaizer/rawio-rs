# rawio

Raw offset read/write CLI for removable block devices (SD cards).

One command per operation, no GUI, no interactive prompts — scriptable on both
Windows 11 and Linux from the same argument syntax.

## Commands

```
rawio list                                  # enumerate candidate devices
rawio probe  <device> [--pit]               # non-destructive pre-flight report
rawio pit    <device>                       # print the PIT partition table
rawio dump   <device> <target> --length N --output FILE
rawio flash  <device> <target> --input FILE [--verify]
rawio verify <device> <target> --input FILE
```

`--offset` / `--length` accept decimal, `0x` hex, and `K`/`M`/`G` suffixes. An
offset that is not a multiple of the device's logical sector size is rejected
rather than rounded. A write whose length is not a multiple of the sector size
reads the final sector back first, so the bytes after the image survive.

`--dry-run` resolves the target, prints what would happen, and stops without
reading or writing the device.

## Targeting

A target is one of `--offset N`, `--partition NAME`, or `--partition-id N`; they
are mutually exclusive. The two partition forms read the PIT, which is otherwise
never touched, and always print the range they resolved to before acting on it.

`rawio pit` prints the whole table, which is where the names and identifiers
come from:

```
pit: read at offset 0 - chip="EMMC16" port="COM4" format="FILE", 2 entries
pit: block size 512 assumed; every byte column below depends on it
device: \\.\PhysicalDrive2  29.7 GiB  removable  sector=512  Generic SD/MMC

  NAME             TYPE     ID   BLOCK OFF    BLOCKS     BYTE OFFSET     BYTE LEN       SIZE  FLASH FILE
  BOOT             mmc       0        2048       128         1048576        65536    64.0 KiB  boot.img
  LOG              mmc       1        8192      1024         4194304       524288   512.0 KiB  -
```

The PIT layout is reverse engineered and its block size is assumed to be 512, so
the byte columns can be plausible and still wrong. An entry that resolves past
the end of the device is flagged, and a partition that resolves past the end
aborts rather than transferring. `--pit-offset N` moves where the table is read
from; the format does not fix its location.

## File paths

`--input` / `--output` are handed to the OS exactly as typed. On Windows that
means the special path forms all work as they do in any other program:

```
\\wsl.localhost\Ubuntu-24.04\home\me\boot.bin   file inside a WSL distribution
\\wsl$\Ubuntu\home\me\boot.bin                  the older WSL share name
\\?\C:\...                                      extended-length path
\\server\share\boot.bin                         plain UNC
```

Reaching a file inside WSL needs that distribution to be running; if it is not,
the failure is reported with the raw Windows error code.

Paths at or beyond Windows' 260 character limit are rewritten into the
extended-length form (`\\?\C:\...`, `\\?\UNC\server\share\...`) just before the
file is opened, so a deeply nested WSL path works without the caller doing
anything. Shorter paths are opened exactly as typed.

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
| 7 | verification failed; the offset of the first differing byte is reported |
