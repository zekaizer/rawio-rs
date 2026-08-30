//! One argument surface, identical on both platforms, with no interactive input
//! anywhere.

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "rawio",
    version,
    about = "Raw offset read/write for removable devices"
)]
pub struct Cli {
    /// Print every device access step. Always printed on failure.
    #[arg(long, global = true)]
    pub trace: bool,

    /// Byte offset the PIT itself sits at. The format does not fix its location,
    /// so it is an argument rather than a guess.
    #[arg(long, value_name = "N", value_parser = parse_size, default_value_t = 0, global = true)]
    pub pit_offset: u64,

    /// Resolve the target and report what would happen, without reading or
    /// writing the device. Applies to dump, flash and verify.
    #[arg(long, global = true)]
    pub dry_run: bool,

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

#[derive(Debug, Args)]
pub struct ProbeArgs {
    pub device: String,

    /// Also read and print the PIT partition table.
    #[arg(long)]
    pub pit: bool,

    #[command(flatten)]
    pub location: Location,
}

#[derive(Debug, Args)]
pub struct PitArgs {
    pub device: String,
}

#[derive(Debug, Args)]
pub struct DumpArgs {
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    /// Bytes to read. Required unless --partition supplies the length.
    #[arg(long, value_parser = parse_size)]
    pub length: Option<u64>,

    /// Destination file.
    #[arg(long, short = 'o')]
    pub output: std::path::PathBuf,
}

#[derive(Debug, Args)]
pub struct FlashArgs {
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    /// Source file. Its length is the write length.
    #[arg(long, short = 'i')]
    pub input: std::path::PathBuf,

    /// Read the range back after writing and compare it with the input.
    #[arg(long)]
    pub verify: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    pub device: String,

    #[command(flatten)]
    pub location: Location,

    /// File the range is compared against. Its length is the compared length.
    #[arg(long, short = 'i')]
    pub input: std::path::PathBuf,
}

/// `--partition` is opt-in and never consulted unless it is given.
#[derive(Debug, Args)]
#[group(required = false, multiple = false)]
pub struct Location {
    /// Byte offset into the device.
    #[arg(long, value_parser = parse_size)]
    pub offset: Option<u64>,

    /// Resolve the range from a PIT partition name instead of --offset.
    #[arg(long, value_name = "NAME")]
    pub partition: Option<String>,

    /// Resolve the range from a PIT partition identifier, the ID column of
    /// `rawio pit`.
    #[arg(long, value_name = "N")]
    pub partition_id: Option<u32>,
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
