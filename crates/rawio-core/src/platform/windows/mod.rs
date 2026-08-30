//! Windows backend. `logic` holds everything that is not a syscall so it stays
//! testable from a Linux or macOS development host.

pub mod logic;

#[cfg(windows)]
mod sys;

use crate::device::{Access, Backend, DeviceInfo, RawDevice, VolumeLock};
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
        #[cfg(windows)]
        {
            sys::enumerate(trace)
        }
        #[cfg(not(windows))]
        {
            let _ = trace;
            Err(host_required(Stage::Enumerate))
        }
    }

    fn open(
        &self,
        id: &str,
        access: Access,
        trace: &Trace,
    ) -> Result<Box<dyn RawDevice>, DeviceError> {
        let index =
            logic::parse_device_id(id).map_err(|message| DeviceError::new(Stage::Open, message))?;
        #[cfg(windows)]
        {
            sys::open(index, access, trace)
        }
        #[cfg(not(windows))]
        {
            let _ = (index, access, trace);
            Err(host_required(Stage::Open))
        }
    }

    fn rehearse_write(&self, id: &str, trace: &Trace) -> Result<Vec<VolumeLock>, DeviceError> {
        let index =
            logic::parse_device_id(id).map_err(|message| DeviceError::new(Stage::Open, message))?;
        #[cfg(windows)]
        {
            sys::rehearse_write(index, trace)
        }
        #[cfg(not(windows))]
        {
            let _ = (index, trace);
            Err(host_required(Stage::Open))
        }
    }
}

#[cfg(not(windows))]
fn host_required(stage: Stage) -> DeviceError {
    DeviceError::new(stage, "the Windows backend requires a Windows host")
}
