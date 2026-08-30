//! Windows refuses a path at or beyond `MAX_PATH` unless it is written in the
//! extended-length form. Nothing else about a path is touched.

use std::path::{Path, PathBuf};

/// Including the terminating NUL, so a usable path is at most 259 characters.
const MAX_PATH: usize = 260;

/// Rewrites an absolute Windows path into its extended-length form when the
/// length demands it. `None` means the path is already usable as it stands.
///
/// Takes and returns strings so the Windows rule is exercised on any host; the
/// caller resolves the path to an absolute one first, because the extended-length
/// form disables the normalisation that would otherwise resolve `.` and `..`.
pub fn extend(absolute: &str) -> Option<String> {
    // Already verbatim, or the device namespace, which is not a file path.
    if absolute.starts_with(r"\\?\") || absolute.starts_with(r"\\.\") {
        return None;
    }
    if absolute.encode_utf16().count() < MAX_PATH {
        return None;
    }
    Some(match absolute.strip_prefix(r"\\") {
        Some(unc) => format!(r"\\?\UNC\{unc}"),
        None => format!(r"\\?\{absolute}"),
    })
}

/// The path to hand to the OS when opening `given`.
pub fn for_open(given: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        // GetFullPathNameW: separators and dot components are resolved here,
        // because the extended-length form would keep them verbatim.
        let Ok(absolute) = std::path::absolute(given) else {
            return given.to_path_buf();
        };
        match extend(&absolute.to_string_lossy()) {
            Some(extended) => PathBuf::from(extended),
            None => given.to_path_buf(),
        }
    }
    #[cfg(not(windows))]
    {
        given.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn long_drive_path(len: usize) -> String {
        let mut path = String::from("C:\\images");
        while path.len() + 1 + 8 <= len {
            path.push_str("\\nested00");
        }
        while path.len() < len {
            path.push('x');
        }
        path
    }

    #[test]
    fn short_paths_are_left_alone() {
        assert_eq!(extend("C:\\images\\boot.bin"), None);
        assert_eq!(
            extend("\\\\wsl.localhost\\Ubuntu\\home\\me\\boot.bin"),
            None
        );
    }

    #[test]
    fn a_path_one_short_of_the_limit_is_still_usable() {
        let path = long_drive_path(MAX_PATH - 1);
        assert_eq!(path.len(), 259);
        assert_eq!(extend(&path), None);
    }

    #[test]
    fn a_drive_path_at_the_limit_gets_the_extended_prefix() {
        let path = long_drive_path(MAX_PATH);
        assert_eq!(extend(&path), Some(format!("\\\\?\\{path}")));
    }

    #[test]
    fn a_unc_path_gets_the_extended_unc_prefix() {
        let tail = "a".repeat(MAX_PATH);
        let path = format!("\\\\wsl.localhost\\Ubuntu\\{tail}");
        assert_eq!(
            extend(&path),
            Some(format!("\\\\?\\UNC\\wsl.localhost\\Ubuntu\\{tail}"))
        );
    }

    #[test]
    fn an_already_extended_path_is_left_alone() {
        let path = format!("\\\\?\\C:\\{}", "a".repeat(MAX_PATH));
        assert_eq!(extend(&path), None);
        let unc = format!("\\\\?\\UNC\\server\\share\\{}", "a".repeat(MAX_PATH));
        assert_eq!(extend(&unc), None);
    }

    #[test]
    fn the_device_namespace_is_left_alone() {
        let path = format!("\\\\.\\PhysicalDrive2\\{}", "a".repeat(MAX_PATH));
        assert_eq!(extend(&path), None);
    }

    /// Windows counts UTF-16 units, not characters.
    #[test]
    fn the_limit_counts_utf16_units() {
        let path = format!("C:\\{}", "\u{1F600}".repeat(130));
        assert_eq!(path.chars().count(), 133);
        assert_eq!(path.encode_utf16().count(), 263);
        assert!(extend(&path).is_some());
    }
}
