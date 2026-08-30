//! Backend selection. Only the syscall layer is `cfg`-gated; every backend's
//! pure logic compiles and is tested on all hosts.

pub mod linux;
pub mod windows;

use crate::device::Backend;
use crate::error::Result;

/// Hosts outside Windows and Linux still build and run the test suite, they
/// just have no device access.
pub fn backend() -> Result<Box<dyn Backend>> {
    #[cfg(windows)]
    {
        Ok(Box::new(windows::WindowsBackend::new()))
    }
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(linux::LinuxBackend::new()))
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        Err(crate::error::Error::Unsupported(std::env::consts::OS))
    }
}
