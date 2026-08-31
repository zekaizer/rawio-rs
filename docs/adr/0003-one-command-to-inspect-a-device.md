# 3. One command to inspect a device

Date: 2026-09-01

## Status

Accepted

Supersedes part of [ADR-1](0001-mbr-and-gpt-partition-tables.md): the `parts`
command and the rule that `--pit-offset` selects the PIT.

## Context

ADR-1 added `rawio parts` beside the existing `rawio probe` and `rawio pit`.
Three commands then inspected a device, and they overlapped: `probe --parts`
printed what `parts` printed, and `parts --scheme pit` printed what `pit`
printed. Which one to run had no good answer, and each carried its own subset
of the same flags.

ADR-1 also decided that `--pit-offset` given explicitly means the PIT, on the
grounds that asking where a table is only makes sense for the format whose
location is not fixed. That reasoning holds for where a PIT is read from. It
does not hold for which table a range is resolved from, and ADR-1 conflated
the two. The consequence it did not foresee is that `--partition-id 1` means
an MBR entry index or a PIT identifier depending on whether an unrelated
offset flag appears on the same line. ADR-1 itself named the hazard - one
selector spelling meaning three different things - and then made which one it
meant unreadable from the command line. A range silently resolving somewhere
plausible is the failure that costs a card.

Separately, whether the OS will yield the volumes a write needs is the most
expensive question in the tool to answer wrong, and it was answered only as a
side effect of `probe`, a command about reporting.

## Decision

`rawio show DEVICE` replaces `probe`, `parts` and `pit`. It prints what the
device is and what it carries. The MBR or GPT costs one to three sector reads,
so it is printed always rather than behind a flag; the PIT costs a search, so
it stays behind `--pit`.

A device carrying no table rawio can read is a finding, not a failure: `show`
prints the reason and exits zero. A layout it cannot conclude from - a hybrid
MBR/GPT, a damaged table, a scheme the caller asserted and the device does not
carry - remains an error. `parts::detect` returns the two as different values
rather than leaving the distinction to an error message.

`--pit-offset` says where a PIT is and nothing else. Only `--scheme` decides
which table a range comes from, because that decision also decides what
`--partition-id` means.

The write rehearsal moves to `flash --dry-run`, so the command that would take
the risk is the one that reports on it.

`--offset`, `--partition` and `--partition-id` stay as three flags. Collapsing
them into one selector was considered and rejected: any single spelling has to
tell an offset from a name, and a partition whose name reads as a number would
then resolve somewhere plausible without saying so - the same failure this ADR
exists to remove.

## Consequences

Every invocation of `probe`, `parts` or `pit` breaks. `parts` was introduced
by ADR-1 and no release has carried it; `probe` and `pit` predate it. This is
a breaking change to the argument surface, taken before the tool has parsed a
real PIT.

A run that resolved a range from the PIT by giving `--pit-offset` alone must
now say `--scheme pit`. The offset keeps its meaning wherever a PIT is read.

`show` reads the partition table on every run, which `probe` did not. Against
that, the question "which of these three do I run" is gone, and so is the
possibility of asking one of them for something only another could answer.

A script that treated a missing partition table as a non-zero exit from
`parts` now has to read `show`'s output instead. Nothing else changes exit
codes: an ambiguous table still exits 3, and a wrong argument still exits 2.
