//! Win32 calls. Compiled only on Windows; everything here is unverified until it
//! runs on real hardware.

use crate::device::{Access, DeviceInfo, RawDevice};
use crate::error::{DeviceError, Stage};

pub fn enumerate() -> Result<Vec<DeviceInfo>, DeviceError> {
    Err(DeviceError::new(Stage::Enumerate, "not implemented yet"))
}

pub fn open(index: u32, access: Access) -> Result<Box<dyn RawDevice>, DeviceError> {
    let _ = (index, access);
    Err(DeviceError::new(Stage::Open, "not implemented yet"))
}
