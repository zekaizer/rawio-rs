//! A record of every device access step, printed on demand and always printed
//! when a run fails.

use std::cell::RefCell;
use std::fmt::Write as _;

use crate::error::{DeviceError, Stage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub stage: Stage,
    pub detail: String,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok(String),
    Failed {
        message: String,
        os_error: Option<i32>,
    },
}

#[derive(Debug, Default)]
pub struct Trace {
    steps: RefCell<Vec<Step>>,
}

impl Trace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn ok(&self, stage: Stage, detail: impl Into<String>, result: impl Into<String>) {
        self.steps.borrow_mut().push(Step {
            stage,
            detail: detail.into(),
            outcome: Outcome::Ok(result.into()),
        });
    }

    pub fn failed(&self, detail: impl Into<String>, err: &DeviceError) {
        self.steps.borrow_mut().push(Step {
            stage: err.stage,
            detail: detail.into(),
            outcome: Outcome::Failed {
                message: err.message.clone(),
                os_error: err.os_error,
            },
        });
    }

    pub fn is_empty(&self) -> bool {
        self.steps.borrow().is_empty()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for step in self.steps.borrow().iter() {
            let _ = write!(out, "  {:<16} {}", step.stage.as_str(), step.detail);
            match &step.outcome {
                Outcome::Ok(result) if result.is_empty() => {
                    let _ = writeln!(out, " -> ok");
                }
                Outcome::Ok(result) => {
                    let _ = writeln!(out, " -> {result}");
                }
                Outcome::Failed {
                    message,
                    os_error: Some(code),
                } => {
                    let _ = writeln!(out, " -> FAILED {message} (os error {code})");
                }
                Outcome::Failed {
                    message,
                    os_error: None,
                } => {
                    let _ = writeln!(out, " -> FAILED {message}");
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_step_reports_stage_and_os_error() {
        let trace = Trace::new();
        trace.ok(Stage::Open, "\\\\.\\PhysicalDrive2", "handle acquired");
        trace.failed(
            "FSCTL_LOCK_VOLUME",
            &DeviceError::with_os_error(Stage::LockVolume, "access denied", 5),
        );

        let rendered = trace.render();
        assert!(rendered.contains("open-device"));
        assert!(rendered.contains("lock-volume"));
        assert!(rendered.contains("FAILED access denied (os error 5)"));
    }
}
