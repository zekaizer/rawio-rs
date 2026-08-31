//! One argument surface, identical on both platforms, with no interactive input
//! anywhere.

use clap::{Args, Parser, Subcommand, ValueEnum};
use rawio_core::pit::DEFAULT_SCAN_BUDGET;

#[derive(Debug, Parser)]
#[command(
    name = "rawio",
    version,
    about = concat!(
        "rawio ",
        env!("CARGO_PKG_VERSION"),
        " - raw offset read/write for removable devices\nby ",
        env!("CARGO_PKG_AUTHORS")
    )
)]
pub struct Cli {
    // The only setting every command can act on, which is why it is the only
    // global one.
    /// Print every device access step. Always printed on failure.
    #[arg(long, global = true)]
    pub trace: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List candidate devices.
    List,
    /// Report everything needed to plan a transfer, without reading or writing.
    Probe(ProbeArgs),
    /// Print the partition table the device carries. Reads only.
    Parts(PartsArgs),
    /// Print the PIT partition table. Reads only.
    Pit(PitArgs),
    /// Print a raw range as a hexdump. Reads only.
    Hex(HexArgs),
    /// Copy a raw range from the device into a file.
    Dump(DumpArgs),
    /// Write a file into a raw range on the device.
    Flash(FlashArgs),
    /// Compare a raw range on the device against a file.
    Verify(VerifyArgs),
}

/// Which table a range comes from. `auto` concludes only what the device
/// proves: a protective entry means GPT, real MBR entries mean MBR, both at
/// once is refused. It never lands on the PIT, whose location is an argument
/// rather than a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SchemeArg {
    Auto,
    Mbr,
    Gpt,
    Pit,
}

/// Where to read the PIT from, for the commands that can read one.
#[derive(Debug, Args)]
pub struct PitSource {
    /// Byte offset the PIT sits at. Without it the table is searched for in
    /// the space no partition covers, nearest the first partition first.
    #[arg(
        long,
        value_name = "N",
        value_parser = parse_size,
        help_heading = "Target"
    )]
    pub pit_offset: Option<u64>,

    /// Bytes of unallocated space the search may read before it gives up.
    /// 0 lifts the cap; a full pass over a large card takes as long as
    /// reading one.
    #[arg(
        long,
        value_name = "N",
        value_parser = parse_size,
        default_value_t = DEFAULT_SCAN_BUDGET,
        help_heading = "Target"
    )]
    pub pit_scan: u64,
}

/// The table a range is resolved from, for every command that resolves one.
#[derive(Debug, Args)]
pub struct TableSource {
    /// Partition table to read: auto, mbr, gpt or pit.
    #[arg(
        long,
        value_enum,
        default_value_t = SchemeArg::Auto,
        value_name = "SCHEME",
        help_heading = "Target"
    )]
    pub scheme: SchemeArg,

    #[command(flatten)]
    pub pit_source: PitSource,
}

/// What the range is, for every command that acts on one. The three forms are
/// mutually exclusive, and the two partition forms are the only thing that
/// makes a partition table be read at all.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct Location {
    /// Byte offset into the device.
    #[arg(long, value_parser = parse_size, value_name = "N", help_heading = "Target")]
    pub offset: Option<u64>,

    /// Resolve the range from a partition name, the NAME column of `rawio parts`
    /// or `rawio pit`. MBR entries have no names.
    #[arg(long, value_name = "NAME", help_heading = "Target")]
    pub partition: Option<String>,

    /// Resolve the range from the ID column of `rawio parts` or `rawio pit`:
    /// the entry index under MBR and GPT, the identifier under a PIT.
    #[arg(long, value_name = "N", help_heading = "Target")]
    pub partition_id: Option<u32>,
}

/// Settings shared by the commands that move or compare bytes, and offered by
/// no others.
#[derive(Debug, Args)]
pub struct TransferOptions {
    /// Resolve the target and report what would happen, without reading or
    /// writing the device.
    #[arg(long)]
    pub dry_run: bool,

    /// Do not draw the progress line. It is drawn only when stderr is a
    /// terminal, so a piped or redirected run is already quiet.
    #[arg(long)]
    pub no_progress: bool,
}

#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// Device to report on, spelled as `rawio list` prints it.
    pub device: String,

    /// Also read and print the partition table the device carries.
    #[arg(long)]
    pub parts: bool,

    /// Also read and print the PIT partition table.
    #[arg(long)]
    pub pit: bool,

    /// A range given here is resolved and printed, never transferred.
    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub table: TableSource,
}

#[derive(Debug, Args)]
pub struct PartsArgs {
    /// Device to read the partition table from, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub table: TableSource,
}

#[derive(Debug, Args)]
pub struct PitArgs {
    /// Device to read the partition table from, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub pit_source: PitSource,
}

/// What a hexdump prints when no length is given: one 512-byte sector, which is
/// what the structures a hexdump gets opened for live in. A partition form
/// supplies the offset, never a length that could be the whole card.
pub const DEFAULT_HEX_LENGTH: u64 = 512;

#[derive(Debug, Args)]
pub struct HexArgs {
    /// Device to read from, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub table: TableSource,

    /// Bytes to print. The offset need not start on a sector.
    #[arg(
        long,
        value_parser = parse_size,
        value_name = "N",
        default_value_t = DEFAULT_HEX_LENGTH,
        help_heading = "Target"
    )]
    pub length: u64,

    /// Print every line, including the runs of identical ones a `*` stands for.
    #[arg(long)]
    pub no_squeeze: bool,

    /// Resolve the range and report it without reading the device.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct DumpArgs {
    /// Device to read from, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub table: TableSource,

    #[command(flatten)]
    pub transfer: TransferOptions,

    /// Bytes to read. Required with --offset; the partition forms supply it.
    #[arg(long, value_parser = parse_size, value_name = "N", help_heading = "Target")]
    pub length: Option<u64>,

    /// Destination file.
    #[arg(long, short = 'o', value_name = "FILE")]
    pub output: std::path::PathBuf,
}

#[derive(Debug, Args)]
pub struct FlashArgs {
    /// Device to write to, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub table: TableSource,

    #[command(flatten)]
    pub transfer: TransferOptions,

    /// Source file. Its length is the write length.
    #[arg(long, short = 'i', value_name = "FILE")]
    pub input: std::path::PathBuf,

    /// Read the range back after writing and compare it with the input.
    #[arg(long)]
    pub verify: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Device to compare against, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub table: TableSource,

    #[command(flatten)]
    pub transfer: TransferOptions,

    /// File the range is compared against. Its length is the compared length.
    #[arg(long, short = 'i', value_name = "FILE")]
    pub input: std::path::PathBuf,
}

/// Accepts decimal, `0x` hex, and K/M/G (1024-based) suffixes.
pub fn parse_size(value: &str) -> Result<u64, String> {
    let text = value.trim().replace('_', "");
    let (digits, shift) = match text.chars().last() {
        Some('K' | 'k') => (&text[..text.len() - 1], 10),
        Some('M' | 'm') => (&text[..text.len() - 1], 20),
        Some('G' | 'g') => (&text[..text.len() - 1], 30),
        _ => (text.as_str(), 0),
    };
    let digits = digits.trim();
    let parsed = match digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))
    {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => digits.parse::<u64>(),
    }
    .map_err(|_| format!("{value:?} is not a byte count"))?;

    parsed
        .checked_shl(shift)
        .filter(|scaled| shift == 0 || scaled >> shift == parsed)
        .ok_or_else(|| format!("{value:?} overflows a 64-bit byte count"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::CommandFactory;

    use super::*;

    /// A build handed over on a USB stick has no other way to say what it is
    /// or who to go back to about it.
    #[test]
    fn help_names_the_version_and_the_author_it_was_built_from() {
        let help = Cli::command().render_long_help().to_string();

        assert!(help.contains(env!("CARGO_PKG_VERSION")), "{help}");
        assert!(help.contains("Luke Lee"), "{help}");
        assert!(help.contains(env!("CARGO_PKG_AUTHORS")), "{help}");
    }

    /// A flag that does nothing on a command should not be offered by it: the
    /// help is the only place these get explained.
    #[test]
    fn options_appear_only_where_they_do_something() {
        let nowhere = [
            (vec!["rawio", "list", "--dry-run"], "list --dry-run"),
            (vec!["rawio", "list", "--no-progress"], "list --no-progress"),
            (
                vec!["rawio", "list", "--pit-offset", "0"],
                "list --pit-offset",
            ),
            (vec!["rawio", "list", "--offset", "0"], "list --offset"),
            (vec!["rawio", "list", "--scheme", "mbr"], "list --scheme"),
            (vec!["rawio", "pit", "d", "--scheme", "mbr"], "pit --scheme"),
            (vec!["rawio", "parts", "d", "--dry-run"], "parts --dry-run"),
            (vec!["rawio", "pit", "d", "--dry-run"], "pit --dry-run"),
            (
                vec!["rawio", "pit", "d", "--no-progress"],
                "pit --no-progress",
            ),
            (
                vec!["rawio", "probe", "d", "--no-progress"],
                "probe --no-progress",
            ),
            (
                vec!["rawio", "hex", "d", "--no-progress"],
                "hex --no-progress",
            ),
            (vec!["rawio", "hex", "d", "-o", "x"], "hex --output"),
        ];
        for (args, what) in nowhere {
            let err = Cli::try_parse_from(&args).unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{what}"
            );
        }
    }

    #[test]
    fn the_options_that_do_something_are_still_there() {
        let somewhere = [
            vec!["rawio", "pit", "d", "--pit-offset", "4K"],
            vec!["rawio", "pit", "d", "--pit-scan", "0"],
            vec!["rawio", "parts", "d"],
            vec!["rawio", "parts", "d", "--scheme", "gpt"],
            vec!["rawio", "probe", "d", "--parts"],
            vec![
                "rawio",
                "dump",
                "d",
                "--scheme",
                "mbr",
                "--partition-id",
                "1",
                "-o",
                "x",
            ],
            vec!["rawio", "hex", "d", "--offset", "0x1be", "--length", "16"],
            vec!["rawio", "hex", "d", "--partition", "BOOT", "--no-squeeze"],
            vec!["rawio", "hex", "d", "--partition-id", "1", "--dry-run"],
            vec!["rawio", "hex", "d", "--offset", "0", "--scheme", "gpt"],
            vec!["rawio", "probe", "d", "--pit", "--pit-offset", "4K"],
            vec!["rawio", "probe", "d", "--partition", "LOG"],
            vec![
                "rawio",
                "dump",
                "d",
                "--offset",
                "0",
                "--length",
                "512",
                "-o",
                "x",
                "--dry-run",
            ],
            vec![
                "rawio",
                "dump",
                "d",
                "--offset",
                "0",
                "--length",
                "512",
                "-o",
                "x",
                "--no-progress",
            ],
            vec![
                "rawio",
                "flash",
                "d",
                "--partition-id",
                "1",
                "-i",
                "x",
                "--verify",
            ],
            vec![
                "rawio",
                "verify",
                "d",
                "--offset",
                "0",
                "-i",
                "x",
                "--dry-run",
            ],
        ];
        for args in somewhere {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
        }
    }

    /// The trace is the one thing every command can produce.
    #[test]
    fn the_trace_is_available_everywhere() {
        let everywhere = [
            vec!["rawio", "list", "--trace"],
            vec!["rawio", "pit", "d", "--trace"],
            vec!["rawio", "parts", "d", "--trace"],
            vec!["rawio", "probe", "d", "--trace"],
            vec![
                "rawio", "dump", "d", "--offset", "0", "--length", "512", "-o", "x", "--trace",
            ],
            vec!["rawio", "hex", "d", "--offset", "0", "--trace"],
            vec!["rawio", "flash", "d", "--offset", "0", "-i", "x", "--trace"],
            vec![
                "rawio", "verify", "d", "--offset", "0", "-i", "x", "--trace",
            ],
        ];
        for args in everywhere {
            assert!(Cli::try_parse_from(&args).is_ok(), "{args:?}");
        }
    }

    #[test]
    fn every_command_says_what_the_device_argument_is() {
        for name in [
            "list", "probe", "parts", "pit", "hex", "dump", "flash", "verify",
        ] {
            let sub = Cli::command()
                .get_subcommands()
                .find(|c| c.get_name() == name)
                .expect("subcommand exists")
                .clone();
            for arg in sub.get_positionals() {
                assert!(
                    arg.get_help().is_some(),
                    "{name} <{}> has no help",
                    arg.get_id()
                );
            }
        }
    }

    /// The default has to be the one that reads what is really there; a wrong
    /// table resolving to a plausible range is what costs a card.
    #[test]
    fn the_scheme_defaults_to_auto() {
        let cli = Cli::try_parse_from(["rawio", "parts", "d"]).unwrap();
        let Command::Parts(args) = cli.command else {
            panic!("expected parts")
        };

        assert_eq!(args.table.scheme, SchemeArg::Auto);
        assert_eq!(args.table.pit_source.pit_offset, None);
        assert_eq!(args.table.pit_source.pit_scan, DEFAULT_SCAN_BUDGET);
    }

    #[test]
    fn every_scheme_is_spelled_the_way_the_output_spells_it() {
        for (given, expected) in [
            ("auto", SchemeArg::Auto),
            ("mbr", SchemeArg::Mbr),
            ("gpt", SchemeArg::Gpt),
            ("pit", SchemeArg::Pit),
        ] {
            let cli = Cli::try_parse_from(["rawio", "parts", "d", "--scheme", given]).unwrap();
            let Command::Parts(args) = cli.command else {
                panic!("expected parts")
            };
            assert_eq!(args.table.scheme, expected, "{given}");
        }
        assert!(Cli::try_parse_from(["rawio", "parts", "d", "--scheme", "ebr"]).is_err());
    }

    /// The offset is what the search exists to avoid needing.
    #[test]
    fn the_pit_offset_is_optional() {
        let cli = Cli::try_parse_from(["rawio", "pit", "d"]).unwrap();
        let Command::Pit(args) = cli.command else {
            panic!("expected pit")
        };

        assert_eq!(args.pit_source.pit_offset, None);
    }

    /// A hexdump is opened to look at one structure; defaulting to a whole
    /// partition would print gigabytes at a terminal.
    #[test]
    fn a_hexdump_prints_one_sector_unless_told_otherwise() {
        let cli = Cli::try_parse_from(["rawio", "hex", "d", "--offset", "0"]).unwrap();
        let Command::Hex(args) = cli.command else {
            panic!("expected hex")
        };

        assert_eq!(args.length, DEFAULT_HEX_LENGTH);
        assert!(!args.no_squeeze);
    }

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn sizes_accept_decimal_hex_and_suffixes() {
        assert_eq!(parse_size("512").unwrap(), 512);
        assert_eq!(parse_size("0x200").unwrap(), 512);
        assert_eq!(parse_size("4K").unwrap(), 4096);
        assert_eq!(parse_size("2M").unwrap(), 2 << 20);
        assert_eq!(parse_size("1g").unwrap(), 1 << 30);
    }

    #[test]
    fn sizes_reject_junk_and_overflow() {
        assert!(parse_size("-1").is_err());
        assert!(parse_size("12x").is_err());
        assert!(parse_size("18446744073709551615G").is_err());
    }

    #[test]
    fn partition_name_and_id_are_mutually_exclusive() {
        let err = Cli::try_parse_from([
            "rawio",
            "dump",
            "d",
            "--partition",
            "LOG",
            "--partition-id",
            "1",
            "-o",
            "out.bin",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    #[test]
    fn offset_and_partition_are_mutually_exclusive() {
        let err = Cli::try_parse_from([
            "rawio",
            "dump",
            "d",
            "--offset",
            "0",
            "--partition",
            "LOG",
            "-o",
            "out.bin",
        ])
        .unwrap_err();
        assert_eq!(err.kind(), clap::error::ErrorKind::ArgumentConflict);
    }

    /// The WSL shares, the extended-length and device namespaces, and plain UNC
    /// all reach the OS exactly as typed. Rewriting them breaks paths that
    /// Windows accepts and we do not.
    #[test]
    fn special_paths_are_not_rewritten() {
        let paths = [
            r"\\wsl.localhost\Ubuntu-24.04\home\me\boot.bin",
            r"\\wsl$\Ubuntu\home\me\boot.bin",
            r"\\?\C:\images\boot.bin",
            r"\\?\UNC\wsl.localhost\Ubuntu\home\me\boot.bin",
            r"\\server\share\boot.bin",
            r"C:\images\boot.bin",
            "C:/images/boot.bin",
            "/home/me/boot.bin",
        ];

        for given in paths {
            let cli = Cli::try_parse_from([
                "rawio", "dump", "dev", "--offset", "0", "--length", "512", "-o", given,
            ])
            .unwrap();
            let Command::Dump(args) = cli.command else {
                panic!("expected dump")
            };
            assert_eq!(args.output, PathBuf::from(given), "{given}");

            let cli = Cli::try_parse_from(["rawio", "flash", "dev", "--offset", "0", "-i", given])
                .unwrap();
            let Command::Flash(args) = cli.command else {
                panic!("expected flash")
            };
            assert_eq!(args.input, PathBuf::from(given), "{given}");
        }
    }

    /// There must be no way to ask for the removable check to be skipped.
    #[test]
    fn no_force_flag_exists() {
        for flag in ["--force", "--yes", "--allow-fixed", "--no-check"] {
            let err =
                Cli::try_parse_from(["rawio", "flash", "d", "--offset", "0", "-i", "x", flag])
                    .unwrap_err();
            assert_eq!(
                err.kind(),
                clap::error::ErrorKind::UnknownArgument,
                "{flag}"
            );
        }
    }
}
