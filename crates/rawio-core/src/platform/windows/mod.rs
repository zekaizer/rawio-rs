//! Windows backend. `logic` holds everything that is not a syscall so it stays
//! testable from a Linux or macOS development host.

pub mod logic;

#[cfg(windows)]
mod sys;

use crate::device::{Access, Backend, DeviceInfo, RawDevice};
use crate::error::{DeviceError, Stage};
use crate::trace::Trace;

#[derive(Debug, Default)]
pub struct WindowsBackend;

impl WindowsBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Backend for WindowsBackend {
    fn enumerate(&self, trace: &Trace) -> Result<Vec<DeviceInfo>, DeviceError> {
        let _ = trace;
        #[cfg(windows)]
        {
            sys::enumerate()
        }
        #[cfg(not(windows))]
        {
            Err(DeviceError::new(
                Stage::Enumerate,
                "Windows backend requires a Windows host",
            ))
        }
    }

    fn open(
        &self,
        id: &str,
        access: Access,
        trace: &Trace,
    ) -> Result<Box<dyn RawDevice>, DeviceError> {
        let _ = trace;
        let index =
            logic::parse_device_id(id).map_err(|message| DeviceError::new(Stage::Open, message))?;
        #[cfg(windows)]
        {
            sys::open(index, access)
        }
        #[cfg(not(windows))]
        {
            let _ = (index, access);
            Err(DeviceError::new(
                Stage::Open,
                "Windows backend requires a Windows host",
            ))
        }
    }
}
