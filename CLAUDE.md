# rawio

Cross-platform CLI that reads and writes raw byte ranges on removable block
devices (SD cards). Windows 11 and Linux, same arguments on both.

## Commands

```
cargo test                                   # host tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo clippy --target x86_64-pc-windows-gnu  # lint the Windows-only paths
```

## Layout

- `src/cli.rs` — argument surface; all parsing and validation, no I/O.
- `src/hexdump.rs` — `hexdump -C` formatting; pure, no device access.
- `src/error.rs` — `Stage`, `DeviceError`, `Error`, exit-code mapping.
- `src/trace.rs` — per-step device access log (`--trace`).
- `src/device.rs` — `RawDevice` / `Backend` traits, `DeviceInfo`, in-memory test double.
- `src/transfer.rs` — dump/flash over the traits: alignment, removable guard, partial-write reporting.
- `src/parts/` — MBR and GPT parsing and scheme detection; read-only.
- `src/pit.rs` — PIT header/entry parsing and its search; read-only, opt-in.
- `src/platform/` — OS backends. `windows/logic.rs` and `linux.rs` pure logic compiles on every host; only `windows/sys.rs` is `#[cfg(windows)]`.

## Rules

- Windows-specific logic must stay testable on a non-Windows host. Anything that
  is not a syscall goes in a module without a `cfg` gate, behind a trait the
  tests can fake. Unit tests passing is not evidence that Windows works.
- Transfers overlap their two sides with a scoped thread and a bounded channel.
  The device stays on the calling thread because `RawDevice` and `Trace` are not
  `Send`; only the file side moves. Buffer recycling is best effort and must
  never be read as a signal to stop.
- A write is not done when the buffer accepts it. `flash` flushes as it goes so
  the progress report tracks the medium; do not move back to a single flush at
  the end.
- Results go to stdout, progress and diagnostics to stderr. A script reads
  stdout, so nothing decorative may land there. How a range resolved - which
  table, which entry, where a PIT search looked and what it found - explains a
  result rather than being one, and goes to stderr with the rest.
- No interactive prompts, ever. Every failure exits non-zero with the stage and
  the raw OS error code.
- `flash` has no removable-check override. Do not add one.
- Locking a volume stops Windows writing over the transfer; only dismounting it
  makes Windows forget the filesystem the transfer just replaced. A real write
  does both. A rehearsal locks, to find out whether it could, and never
  dismounts.
- `--input` / `--output` reach the OS verbatim. Do not rewrite them: the WSL
  shares, `\\?\` and UNC are all valid Windows paths that any translation
  layer would break. Device arguments keep their own per-backend syntax. The one
  exception is `longpath::extend`, which adds the extended-length prefix when a
  path is at or beyond `MAX_PATH` and would otherwise be rejected outright.
- Every command that acts on a range must be able to show what it resolved to
  without acting: `--dry-run` on hex, dump, flash and verify, and `rawio show`
  for the tables. There are no interactive prompts to fall back on.
- A command that acts on a range is refused without one while it is still an
  argument list. Opening for write locks and dismounts every volume on the
  card, so a usage error must never get that far. `show` is the only command
  that may be handed a device and nothing else.
- `rawio show` is the only command that inspects. It prints the MBR or GPT
  always, because that costs a sector or two, and the PIT only under `--pit`,
  because that costs a search. A device carrying no table it can read is a
  finding it prints and exits zero on; a layout it cannot conclude from is an
  error. `parts::detect` keeps those two apart as values, not as messages.
- `flash --dry-run` rehearses the write: it takes the writable handle and the
  locks, without the dismount, and releases both. It is the only rehearsal,
  and it is only reached once the removable check has passed.
- `rawio hex` is `hexdump -C` byte for byte, so its output diffs against the
  tool the reader already has. Its length defaults to one sector even when a
  partition supplies the offset; a partition is never printed whole by default,
  unless it is shorter than a sector and the whole of it is less. A length the
  caller did give is checked against the entry and named in the message; the
  default one is not, because they never typed it.
- Scheme detection concludes only what a signature proves, and never lands on
  the PIT. A card carrying both a real MBR and a GPT aborts asking for an
  explicit `--scheme` rather than preferring one. `--pit-offset` says where a
  PIT is and nothing more: only `--scheme` decides which table a range comes
  from, because that decision also decides what `--partition-id` means.
- A PIT is searched for only in the space no partition covers, and the gap in
  front of the first partition is searched backwards from that partition. The
  budget exists because a full pass costs what reading the whole card costs, so
  lifting it takes `--pit-scan all` and never a bare 0.
- The PIT layout is reverse engineered, not specified. Two sources agree on it
  (an XDA analysis and github.com/CruelKernel/samsung_pit), but no real card has
  been parsed yet. Keep it opt-in, keep printing the header and the resolved
  range before using it, and abort on any parse failure. The 512B block size is
  still an assumption.
