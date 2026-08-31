//! Failure classification. Every failure carries the stage it happened in and,
//! where the OS produced one, the raw error code - a single run's output has to
//! be enough to locate a failure without reproducing it.

use std::fmt;

/// Device access steps reported by `--trace` and named in every `DeviceError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Enumerate,
    Open,
    QueryGeometry,
    LockVolume,
    Seek,
    Read,
    Write,
    Flush,
    ParsePit,
    ParseParts,
}

impl Stage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Stage::Enumerate => "enumerate-devices",
            Stage::Open => "open-device",
            Stage::QueryGeometry => "query-geometry",
            Stage::LockVolume => "lock-volume",
            Stage::Seek => "seek",
            Stage::Read => "read",
            Stage::Write => "write",
            Stage::Flush => "flush",
            Stage::ParsePit => "parse-pit",
            Stage::ParseParts => "parse-partitions",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A failed device access. `os_error` is the platform's own code (Win32 error
/// or errno), kept unmapped so the value in the output matches OS documentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceError {
    pub stage: Stage,
    pub message: String,
    pub os_error: Option<i32>,
}

impl DeviceError {
    pub fn new(stage: Stage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            os_error: None,
        }
    }

    pub fn with_os_error(stage: Stage, message: impl Into<String>, os_error: i32) -> Self {
        Self {
            stage,
            message: message.into(),
            os_error: Some(os_error),
        }
    }

    pub fn from_io(stage: Stage, err: &std::io::Error) -> Self {
        let os_error = err.raw_os_error();
        let text = err.to_string();
        // `io::Error` renders the code into its own message; `Display` adds it back.
        let message = match os_error {
            Some(code) => text
                .trim_end_matches(&format!(" (os error {code})"))
                .to_string(),
            None => text,
        };
        Self {
            stage,
            message,
            os_error,
        }
    }
}

impl fmt::Display for DeviceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.stage, self.message)?;
        match self.os_error {
            Some(code) => write!(f, " (os error {code})"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for DeviceError {}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Device(#[from] DeviceError),

    /// Fixed disks are rejected before any write is attempted.
    #[error("refusing to write: {device} is not a removable device ({classification})")]
    NotRemovable {
        device: String,
        classification: &'static str,
    },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("PIT: {0}")]
    Pit(String),

    #[error("partition table: {0}")]
    Parts(String),

    /// The device is left partially written; `written` bytes landed at `start`.
    #[error("write aborted after {written} bytes written at offset {start}: {source}")]
    WriteAborted {
        start: u64,
        written: u64,
        source: DeviceError,
    },

    /// The medium did not keep what was written to it.
    #[error("verify failed at offset {offset}: expected {expected:#04x}, found {found:#04x}")]
    VerifyFailed {
        offset: u64,
        expected: u8,
        found: u8,
    },

    #[error("not supported on this platform: {0}")]
    Unsupported(&'static str),

    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
}

impl Error {
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// The exit code alone must separate the failure classes a script cares about.
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::InvalidArgument(_) => 2,
            Error::Device(_) | Error::Io { .. } | Error::Pit(_) | Error::Parts(_) => 3,
            Error::NotRemovable { .. } => 4,
            Error::WriteAborted { .. } => 5,
            Error::Unsupported(_) => 6,
            Error::VerifyFailed { .. } => 7,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_error_display_carries_stage_and_os_code() {
        let err = DeviceError::with_os_error(Stage::LockVolume, "access denied", 5);
        assert_eq!(err.to_string(), "[lock-volume] access denied (os error 5)");
    }

    /// `io::Error` already renders the code into its own message, so the raw
    /// value has to be taken out before `Display` puts it back.
    #[test]
    fn an_os_error_is_reported_once() {
        let err = DeviceError::from_io(
            Stage::Open,
            &std::io::Error::from_raw_os_error(libc_eacces()),
        );

        assert_eq!(err.os_error, Some(libc_eacces()));
        assert!(!err.message.contains("os error"), "{}", err.message);
        assert_eq!(err.to_string().matches("os error").count(), 1, "{err}");
    }

    fn libc_eacces() -> i32 {
        13
    }

    #[test]
    fn exit_codes_separate_failure_classes() {
        let cases = [
            (Error::InvalidArgument("x".into()), 2u8),
            (Error::Device(DeviceError::new(Stage::Open, "x")), 3),
            (
                Error::NotRemovable {
                    device: "d".into(),
                    classification: "fixed",
                },
                4,
            ),
            (
                Error::WriteAborted {
                    start: 0,
                    written: 512,
                    source: DeviceError::new(Stage::Write, "x"),
                },
                5,
            ),
            (Error::Unsupported("x"), 6),
            (
                Error::VerifyFailed {
                    offset: 4196,
                    expected: 0x11,
                    found: 0x22,
                },
                7,
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.exit_code(), expected, "{err}");
        }
    }
}
