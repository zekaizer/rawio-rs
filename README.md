# rawio

Raw offset read/write CLI for removable block devices (SD cards).

One command per operation, no GUI, no interactive prompts — scriptable on both
Windows 11 and Linux from the same argument syntax.

## Commands

```
rawio list                                  # enumerate candidate devices
rawio show   <device> [--pit]               # what the device is and what it carries
rawio hex    <device> <target> [--length N] # print a raw range as a hexdump
rawio dump   <device> <target> --length N --output FILE
rawio flash  <device> <target> --input FILE [--verify]
rawio verify <device> <target> --input FILE
```

`--offset` / `--length` accept decimal, `0x` hex, and `K`/`M`/`G` suffixes. An
offset that is not a multiple of the device's logical sector size is rejected
rather than rounded. A write whose length is not a multiple of the sector size
reads the final sector back first, so the bytes after the image survive.

`--dry-run` resolves the target, prints what would happen, and moves no bytes.
On `flash` it also rehearses the volume locks the write would need. It is offered by `hex`, `dump`, `flash` and
`verify` and by nothing else; `--no-progress` by the three that transfer, while
`--scheme`, `--pit-offset` and `--pit-scan` are offered only by the commands
that read a table. `--trace` is the one option every command takes.

A progress line is drawn on stderr while a transfer runs, and only when stderr
is a terminal, so a piped or redirected run stays quiet on its own. `--no-progress`
turns it off everywhere. Results go to stdout, so a script can read them with
the progress line still on screen.

```
flash 32.0 MiB / 512.0 MiB    6%  16.0 MiB/s  30s
```

`flash` pushes to the medium as it goes rather than only at the end, so the
line tracks what the card has taken and not what the page cache has absorbed.
The last push is reported as a wait, because it is one.

## Targeting

A target is one of `--offset N`, `--partition NAME`, or `--partition-id N`; they
are mutually exclusive. The two partition forms read a partition table, which is
otherwise never touched, and always print the range they resolved to before
acting on it.

`--scheme` says which table that is:

| `--scheme` | Table |
|---|---|
| `auto` (default) | whatever the card proves it has, MBR or GPT |
| `mbr` | the MBR at LBA 0, including logical partitions on the EBR chain |
| `gpt` | the GPT at LBA 1, or its backup at the last LBA |
| `pit` | the Samsung PIT, wherever it is |

`auto` concludes only what a signature proves. A protective entry means GPT; an
MBR carrying real entries means MBR; a card carrying both is a hybrid layout and
the run aborts asking for an explicit `--scheme`. It never lands on a PIT, whose
location is an argument rather than a constant — `--scheme pit` or `--pit-offset`
is what asks for one.

`--partition NAME` matches a GPT name or a PIT name. MBR entries have no names,
so there `--partition-id N` is the only selector, and it means the entry index —
1 to 4 for primaries, 5 up for logical partitions, as Linux numbers them. Under
a PIT it means the identifier instead.

`rawio show` prints what the device is, the table it carries and the space that
table leaves over:

```
device: \\.\PhysicalDrive2  29.7 GiB  removable  sector=512  Generic SD/MMC
writable: true
parts: scheme=gpt, from primary GPT header at LBA 1, 2 entries

   ID  NAME                     TYPE                                            START         LENGTH       SIZE
    1  boot                     c12a7328-f81f-11d2-ba4b-00a0c93ec93b          1048576      268435456  256.0 MiB
    2  rootfs                   0fc63daf-8483-4772-8e79-3d69d8477de4        269484032    15568207872   14.5 GiB

  unallocated 0..1048576 (1.0 MiB)  << a PIT search looks here first, backwards
  unallocated 15837691904..15931539456 (89.5 MiB)
```

Partition types are printed as the table spells them — a hex byte under MBR, the
type GUID under GPT — with no built-in name table to fall out of date. GPT is
checked by CRC32 on both the header and the entry array, and falls back to the
backup header, saying so when it does.

## Looking at bytes

`rawio hex` prints a range the way `hexdump -C` prints it — same columns, same
`*` for a run of identical lines, same closing offset — so its output diffs
against the tool every reader already has:

```
$ rawio hex \\.\PhysicalDrive2 --offset 0 --length 64
00000000  eb 3c 90 4d 53 44 4f 53  35 2e 30 00 02 08 20 00  |.<.MSDOS5.0... .|
00000010  02 00 02 00 00 f8 00 00  3f 00 ff 00 00 00 00 00  |........?.......|
00000020  00 00 00 00 00 00 00 00  00 00 00 00 00 00 00 00  |................|
*
00000040
```

It takes every target form the transfer commands take, so a partition can be
looked at by name or id without working out where it starts:

```
$ rawio hex \\.\PhysicalDrive2 --partition boot --length 32
parts: gpt #1 boot spans 1048576..269484032 (268435456 bytes), type c12a7328-f81f-11d2-ba4b-00a0c93ec93b, from primary GPT header at LBA 1
00100000  eb 58 90 6d 6b 66 73 2e  66 61 74 00 02 08 20 00  |.X.mkfs.fat... .|
00100010  02 00 00 00 00 f8 00 00  3f 00 ff 00 00 08 00 00  |........?.......|
00100020
```

Unlike a transfer the offset need not be a multiple of the sector size: the
sector it falls in is read whole and the head discarded, so a structure can be
looked at where it actually starts (`--offset 0x1be` for the first MBR entry).
The length defaults to one 512-byte sector, including when a partition supplies
the offset, so naming a 16 GiB partition never prints 16 GiB; an entry shorter
than a sector is printed whole. `--no-squeeze`
prints the lines a `*` stands for, and `--dry-run` reports the range it resolved
without reading the device.

Nothing decorative is added: the hexdump is the result and it has stdout to
itself. The `parts:`/`pit:` line that says what a partition name resolved to
explains the result rather than being one, so it takes stderr.

## The PIT

`rawio show --pit` prints the whole table, which is where its names and
identifiers come from:

```
pit: read at offset 1048064 - chip="EMMC16" port="COM4" format="FILE", 2 entries
pit: block size 512 assumed; every byte column below depends on it
device: \\.\PhysicalDrive2  29.7 GiB  removable  sector=512  Generic SD/MMC

  NAME             TYPE     ID   BLOCK OFF    BLOCKS     BYTE OFFSET     BYTE LEN       SIZE  FLASH FILE
  BOOT             mmc       0        2048       128         1048576        65536    64.0 KiB  boot.img
  LOG              mmc       1        8192      1024         4194304       524288   512.0 KiB  -
```

The PIT layout is reverse engineered and its block size is assumed to be 512, so
the byte columns can be plausible and still wrong. An entry that resolves past
the end of the device is flagged, and a partition that resolves past the end
aborts rather than transferring.

Nothing points at the PIT and its format does not fix where it lives, so without
`--pit-offset N` it is searched for: the sectors no partition covers, taken from
whichever table the card carries, with the space in front of the first partition
read **backwards** — the sector immediately before that partition first, then the
one before it. That is where the table sits on the card this was built for.

```
pit: searching 0..1048576 backwards, 15837691904..15931539456
pit: found at offset 1048064 by searching the space no partition covers
```

Both lines go to stderr: they say how the table was arrived at, not what it is.

A magic hit is only a candidate: the header is parsed, and one that does not
parse is passed over. The search reads at most `--pit-scan` bytes, 64 MiB by
default, because a full pass over a card costs what reading the card costs —
64 GiB at 30 MB/s is over half an hour. `--pit-scan all` lifts the cap; a bare
`0` is refused rather than read as either "no cap" or "read nothing".

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

## Writing under a mounted volume

On Windows a write through a physical disk handle is refused where a mounted
volume covers those sectors, unless that volume is locked. `flash` locks every
volume the OS has mounted on the target device and holds the lock until it is
done, then dismounts it: a lock stops Windows writing over the transfer, but
only a dismount makes it forget the filesystem it had cached, which a raw write
has just replaced. Sectors outside every mounted volume are writable either way.

A lock needs exclusive access and fails if anything holds a file open on that
volume, so `flash --dry-run` rehearses the whole thing - writable handle, then
the locks, without the dismount - and releases it again without writing. That
answers whether the flash would be permitted before a card is at stake:

```
$ rawio flash \\.\PhysicalDrive2 --partition boot -i boot.img --dry-run
dry-run: would write 65536 bytes from "boot.img" to \\.\PhysicalDrive2 at 1048576..1114112
write rehearsal: writable handle taken
  volume \\.\E: locked
  volume \\.\F: NOT locked - [lock-volume] access denied (os error 5)
```

A device that is not removable is refused before any of this: there is nothing
to rehearse about a write that cannot happen.

## Privileges

Opening a physical disk needs privilege on both platforms, and rawio cannot take
it for itself: a process cannot elevate its own token, and the elevated process a
UAC prompt would start gets its own console, so nothing it prints comes back to
the shell that asked for it. Run it from an elevated shell. Windows 11's
`sudo.exe` works when it is configured for inline mode, which keeps stdout in the
same console; the default new-window mode does not. `list` still works
unelevated, through a query-only handle, and anything else fails with
`access denied - run elevated` and exit code 3.

On Linux the same access is a file permission: root, or a udev rule that hands
the device to a group you are in.

## Safety

`flash` refuses any device that is not reported as removable. There is no
override flag. This blocks fixed disks; it does **not** distinguish a USB SD card
reader from a USB external SSD — verify the target with `list` / `show` first.

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

Publishing a GitHub release builds both targets on their own runner, runs the
test suite there first, and attaches the binaries to the release. They are named
`rawio-<tag>-<target>`, because these get carried between machines on a stick
and the file has to say which build it is.

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
