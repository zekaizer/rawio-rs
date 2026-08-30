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
- `src/error.rs` — `Stage`, `DeviceError`, `Error`, exit-code mapping.
- `src/trace.rs` — per-step device access log (`--trace`).
- `src/device.rs` — `RawDevice` / `Backend` traits, `DeviceInfo`, in-memory test double.
- `src/transfer.rs` — dump/flash over the traits: alignment, removable guard, partial-write reporting.
- `src/pit.rs` — PIT header/entry parsing; read-only, opt-in.
- `src/platform/` — OS backends. `windows/logic.rs` and `linux.rs` pure logic compiles on every host; only `windows/sys.rs` is `#[cfg(windows)]`.

## Rules

- Windows-specific logic must stay testable on a non-Windows host. Anything that
  is not a syscall goes in a module without a `cfg` gate, behind a trait the
  tests can fake. Unit tests passing is not evidence that Windows works.
- A write is not done when the buffer accepts it. `flash` flushes as it goes so
  the progress report tracks the medium; do not move back to a single flush at
  the end.
- Results go to stdout, progress and diagnostics to stderr. A script reads
  stdout, so nothing decorative may land there.
- No interactive prompts, ever. Every failure exits non-zero with the stage and
  the raw OS error code.
- `flash` has no removable-check override. Do not add one.
- `--input` / `--output` reach the OS verbatim. Do not rewrite them: the WSL
  shares, `\\?\` and UNC are all valid Windows paths that any translation
  layer would break. Device arguments keep their own per-backend syntax. The one
  exception is `longpath::extend`, which adds the extended-length prefix when a
  path is at or beyond `MAX_PATH` and would otherwise be rejected outright.
- Every command that acts on a range must be able to show what it resolved to
  without acting: `--dry-run` on dump, flash and verify, and `rawio pit` for the
  table. There are no interactive prompts to fall back on.
- The PIT layout is reverse engineered, not specified. Two sources agree on it
  (an XDA analysis and github.com/CruelKernel/samsung_pit), but no real card has
  been parsed yet. Keep it opt-in, keep printing the header and the resolved
  range before using it, and abort on any parse failure. The 512B block size is
  still an assumption.
