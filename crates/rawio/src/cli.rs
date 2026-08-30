//! One argument surface, identical on both platforms, with no interactive input
//! anywhere.

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "rawio",
    version,
    about = concat!(
        "rawio ",
        env!("CARGO_PKG_VERSION"),
        " - raw offset read/write for removable devices"
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
    /// Print the PIT partition table. Reads only.
    Pit(PitArgs),
    /// Copy a raw range from the device into a file.
    Dump(DumpArgs),
    /// Write a file into a raw range on the device.
    Flash(FlashArgs),
    /// Compare a raw range on the device against a file.
    Verify(VerifyArgs),
}

/// Where to read the PIT from, for the commands that can read one.
#[derive(Debug, Args)]
pub struct PitSource {
    /// Byte offset the PIT itself sits at. The format does not fix its
    /// location, so it is an argument rather than a guess.
    #[arg(
        long,
        value_name = "N",
        value_parser = parse_size,
        default_value_t = 0,
        help_heading = "Target"
    )]
    pub pit_offset: u64,
}

/// What the range is, for every command that acts on one. The three forms are
/// mutually exclusive, and the two partition forms are the only thing that
/// makes the PIT be read at all.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct Location {
    /// Byte offset into the device.
    #[arg(long, value_parser = parse_size, value_name = "N", help_heading = "Target")]
    pub offset: Option<u64>,

    /// Resolve the range from a PIT partition name, the NAME column of `rawio pit`.
    #[arg(long, value_name = "NAME", help_heading = "Target")]
    pub partition: Option<String>,

    /// Resolve the range from a PIT partition identifier, the ID column of `rawio pit`.
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

    /// Also read and print the PIT partition table.
    #[arg(long)]
    pub pit: bool,

    /// A range given here is resolved and printed, never transferred.
    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub pit_source: PitSource,
}

#[derive(Debug, Args)]
pub struct PitArgs {
    /// Device to read the partition table from, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub pit_source: PitSource,
}

#[derive(Debug, Args)]
pub struct DumpArgs {
    /// Device to read from, spelled as `rawio list` prints it.
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    #[command(flatten)]
    pub pit_source: PitSource,

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
    pub pit_source: PitSource,

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
    pub pit_source: PitSource,

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

    /// A build handed over on a USB stick has no other way to say what it is.
    #[test]
    fn help_names_the_version_it_was_built_from() {
        let help = Cli::command().render_long_help().to_string();

        assert!(help.contains(env!("CARGO_PKG_VERSION")), "{help}");
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
            (vec!["rawio", "pit", "d", "--dry-run"], "pit --dry-run"),
            (
                vec!["rawio", "pit", "d", "--no-progress"],
                "pit --no-progress",
            ),
            (
                vec!["rawio", "probe", "d", "--no-progress"],
                "probe --no-progress",
            ),
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
            vec!["rawio", "probe", "d", "--trace"],
            vec![
                "rawio", "dump", "d", "--offset", "0", "--length", "512", "-o", "x", "--trace",
            ],
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
        for name in ["list", "probe", "pit", "dump", "flash", "verify"] {
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
