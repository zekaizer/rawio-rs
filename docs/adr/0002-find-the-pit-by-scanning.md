# 2. Find the PIT by scanning the space no partition covers

Date: 2026-08-31

## Status

Accepted

## Context

`--pit-offset` was a required argument because the PIT format does not fix
where its table lives. On the first real card to carry one, the table sits at a
sector boundary in the unallocated space in front of the first MBR partition,
outside every partition the MBR declares. Nothing in the MBR points at it.

Asking the user for an offset they would have to find by hand is the worst of
the options available. Searching for it is possible - the table opens with a
four-byte magic and can only start on a sector boundary - but a full pass over
a card is bounded by read throughput, not by the search: 64 GiB at the 30 MB/s
a USB reader manages is about half an hour. A search that reads the whole card
by default is not a default.

Two facts make a bounded search practical. The table is not inside a partition,
so the space worth reading is only what the partitions leave over - typically
the 1 MiB in front of the first partition plus whatever follows the last one.
And it is tucked against the partition that follows it, not against LBA 0.

## Decision

`--pit-offset` becomes optional. Without it, the PIT is found by searching the
gaps the partition table leaves - the complement of the partition ranges,
computed the same way whether the table is MBR or GPT.

The gap in front of the first partition is searched **backwards**: the first
sector examined is the one immediately before the first partition, then the one
before that. On the layout in hand this finds the table in the first block
read.

Reads are batched into blocks of up to 4 MiB, which changes nothing about the
order candidates are examined in. A magic hit is only a candidate: the header is
parsed, and a hit that does not parse is passed over rather than reported, since
four bytes come up by chance.

The search reads at most `--pit-scan` bytes, defaulting to 64 MiB, with `0`
lifting the cap. Where the table was found is printed before anything acts on a
range it resolves.

## Consequences

`rawio pit <device>` now works without knowing where the table is, which is the
only way it is usable on a card nobody has mapped yet.

A card whose PIT lies deep in a large unallocated tail needs `--pit-scan 0` and
the read time that implies. The failure message names both `--pit-offset` and
`--pit-scan`, because there is no prompt to fall back on.

A search cannot prove it found the right copy. Two tables in the same gap
resolve to whichever sits closer to the first partition, and the offset is
printed for exactly that reason - the same reason the resolved range is printed
before a transfer.

The gap set is computed from a partition table that must itself be read first,
so a PIT on a card with no MBR or GPT is searched for from offset 0 up to the
budget, and one on a card whose table is corrupt is not searched for at all
until `--pit-offset` says where to look.
