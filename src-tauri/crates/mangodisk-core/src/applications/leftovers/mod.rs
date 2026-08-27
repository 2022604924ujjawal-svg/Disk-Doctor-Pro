#[cfg(target_os = "macos")]
mod macos;
mod models;
mod service;

pub use models::{
    ApplicationLeftoverActionReason, ApplicationLeftoverActionResult,
    ApplicationLeftoverActionStatus, ApplicationLeftoverCandidate, ApplicationLeftoverConfidence,
    ApplicationLeftoverEvidence, ApplicationLeftoverPlan, ApplicationLeftoverPlanItem,
    ApplicationLeftoverResult, ApplicationLeftoverScanResult, ApplicationLeftoverSource,
};
pub use service::ApplicationLeftoverService;
