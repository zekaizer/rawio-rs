# 1. MBR and GPT partition tables

Date: 2026-08-31

## Status

Superseded in part by [ADR-3](0003-one-command-to-inspect-a-device.md): the
`parts` command and the `--pit-offset` detection rule. Everything else here -
`--scheme`, the MBR/GPT detection rules, the hybrid refusal, and how each table
is validated and printed - still holds.

## Context

Until now the only partition table rawio could resolve a range from was the
PIT, so `--partition` and `--partition-id` always meant "look in the PIT". The
PIT is a reverse-engineered Samsung format that no card in hand has yet been
parsed from; MBR and GPT are the tables that every ordinary SD card actually
carries. Making a partition addressable by name or index is worth much more
when the table is the one that is really there.

Adding a second and third table source forces a decision the single-source
design never had to make: given `--partition boot`, which table is read? Three
properties constrain the answer.

- A range must be printable without acting on it, and there are no interactive
  prompts to fall back on. Whatever picks the table has to be able to say what
  it picked and why.
- The PIT has no fixed location - `--pit-offset` exists because the format does
  not say where the table sits - so nothing can be detected about it by reading
  a well-known sector. MBR and GPT are the opposite: both live at a fixed LBA
  and both carry a signature.
- A wrong table silently resolving to a plausible-looking range is the failure
  that costs a card. Ambiguity has to be an error, not a preference.

## Decision

`--scheme auto|mbr|gpt|pit` selects the table, defaulting to `auto`, and a new
`rawio parts` command prints the resolved table the way `rawio pit` prints the
PIT.

`auto` detects only what can be detected:

- `--pit-offset` given explicitly means the PIT, because asking where the table
  is only makes sense for the one format whose location is not fixed.
- A protective MBR entry (type `0xEE`) means GPT.
- Otherwise an MBR signature with at least one non-empty entry means MBR, and
  an MBR signature with none means whatever GPT is at LBA 1.
- An MBR carrying real entries *and* a GPT signature at LBA 1 is a hybrid
  layout, and `auto` refuses it: the run aborts asking for an explicit
  `--scheme`.

`rawio pit` stays as it is, PIT-only. The `Pit` type keeps its own shape -
identifiers, block units, the 512B assumption - rather than being folded into
the MBR/GPT partition type, because the columns worth printing differ and the
PIT's are the ones that need the loudest caveats.

GPT is validated by CRC32 on both the header and the entry array, and falls
back to the backup header when the primary fails; which one was used is
printed. MBR follows the EBR chain so logical partitions are addressable, and
numbers them from 5 as Linux does.

Partition types are printed raw - a hex byte for MBR, the type GUID for GPT -
with no built-in name table to fall out of date.

## Consequences

`--partition NAME` without `--scheme` no longer means the PIT. A run that
relied on that now needs `--scheme pit`, or `--pit-offset`, which the PIT's own
lack of a fixed location makes the natural spelling anyway. This is a breaking
change to the argument surface, taken before the tool has parsed a real PIT.

`auto` costs one to three sector reads before any transfer, and can fail on a
hybrid layout that an explicit `--scheme` would have handled. Both are the
price of never guessing which table to trust.

MBR partitions have no names, so `--partition NAME` is an error against an MBR
and `--partition-id` means the index there. One selector spelling therefore
means three different things depending on the scheme, which the `parts` output
has to make obvious: it prints the scheme, where the table was read from, and
the resolved byte range before anything acts on it.
