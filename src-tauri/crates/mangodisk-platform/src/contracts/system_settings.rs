use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[cfg(any(target_os = "macos", test))]
use super::PlatformError;
use super::{PlatformCancellation, PlatformResult};

/// A typed value crossing the Core/platform boundary for one known setting.
/// Native storage details remain private to the operating-system adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum PlatformSystemSettingValue {
    Missing,
    Boolean(bool),
    Integer(i64),
    Text(String),
    /// An exact native snapshot used only for optimistic concurrency and durable recovery.
    /// Product status is derived from `PlatformSystemSettingState::effective_value` instead.
    Snapshot(PlatformSystemSettingSnapshot),
}

/// Exact representations for settings that cannot be safely modeled as one scalar value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum PlatformSystemSettingSnapshot {
    Text(String),
    IntegerMap(BTreeMap<String, Option<i64>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlatformSystemSettingDiagnosticCode {
    AccessDenied,
    InvalidData,
    Unsupported,
    StateUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformSystemSettingState {
    pub setting_id: String,
    /// Exact value retained for concurrency checks and recovery.
    pub value: PlatformSystemSettingValue,
    /// Logical scalar compared with the product catalog's recommended value.
    pub effective_value: PlatformSystemSettingValue,
    /// Reports the native capability boundary instead of asking Core to infer it from an ID.
    pub requires_elevation: bool,
    pub diagnostic: Option<PlatformSystemSettingDiagnosticCode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformSystemSettingChangeRequest {
    pub setting_id: String,
    pub expected_value: PlatformSystemSettingValue,
    pub desired_value: PlatformSystemSettingValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlatformSystemSettingChangeResult {
    pub value: PlatformSystemSettingValue,
    pub changed: bool,
    pub verified: bool,
}

/// Evaluates the optimistic-concurrency boundary shared by native adapters.
/// A value already equal to the desired state is a verified idempotent success,
/// which also lets crash recovery discard entries that were already restored.
/// Every other value must still match the scan snapshot before it can be changed.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn preflight_system_setting_change(
    current_value: &PlatformSystemSettingValue,
    request: &PlatformSystemSettingChangeRequest,
) -> PlatformResult<Option<PlatformSystemSettingChangeResult>> {
    if current_value == &request.desired_value {
        return Ok(Some(PlatformSystemSettingChangeResult {
            value: current_value.clone(),
            changed: false,
            verified: true,
        }));
    }
    if current_value != &request.expected_value {
        return Err(PlatformError::item_changed(
            "system setting changed after plan creation",
        ));
    }
    Ok(None)
}

/// Exposes only the finite setting catalog compiled into MangoDisk. Adapters
/// must reject unknown identifiers so input can never select an arbitrary
/// preferences domain, registry path, or command.
pub trait SystemSettingsPlatform: Send + Sync {
    fn scan_system_settings(
        &self,
        setting_ids: &[&str],
        cancellation: &PlatformCancellation,
    ) -> PlatformResult<Vec<PlatformSystemSettingState>>;

    fn change_system_setting(
        &self,
        request: &PlatformSystemSettingChangeRequest,
    ) -> PlatformResult<PlatformSystemSettingChangeResult>;

    /// Applies a prepared batch while preserving the result order.
    ///
    /// Most adapters can safely use the per-item fallback. Windows overrides this boundary so all
    /// machine-scoped settings share one short-lived elevated helper instead of showing one UAC
    /// prompt for every selected item.
    fn change_system_settings(
        &self,
        requests: &[PlatformSystemSettingChangeRequest],
    ) -> PlatformResult<Vec<PlatformResult<PlatformSystemSettingChangeResult>>> {
        Ok(requests
            .iter()
            .map(|request| self.change_system_setting(request))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        expected_value: PlatformSystemSettingValue,
        desired_value: PlatformSystemSettingValue,
    ) -> PlatformSystemSettingChangeRequest {
        PlatformSystemSettingChangeRequest {
            setting_id: "test.setting".to_string(),
            expected_value,
            desired_value,
        }
    }

    #[test]
    fn preflight_accepts_an_already_desired_value_idempotently() {
        let request = request(
            PlatformSystemSettingValue::Integer(0),
            PlatformSystemSettingValue::Integer(1),
        );

        let result =
            preflight_system_setting_change(&PlatformSystemSettingValue::Integer(1), &request)
                .expect("the desired value should be accepted")
                .expect("the preflight should finish without a write");

        assert!(!result.changed);
        assert!(result.verified);
        assert_eq!(result.value, PlatformSystemSettingValue::Integer(1));
    }

    #[test]
    fn preflight_rejects_value_drift() {
        let request = request(
            PlatformSystemSettingValue::Integer(0),
            PlatformSystemSettingValue::Integer(1),
        );

        let error =
            preflight_system_setting_change(&PlatformSystemSettingValue::Integer(2), &request)
                .expect_err("an unrelated current value must fail closed");

        assert_eq!(error.code(), super::super::PlatformErrorCode::ItemChanged);
    }

    #[test]
    fn preflight_allows_a_matching_snapshot_to_continue() {
        let request = request(
            PlatformSystemSettingValue::Integer(0),
            PlatformSystemSettingValue::Integer(1),
        );

        let result =
            preflight_system_setting_change(&PlatformSystemSettingValue::Integer(0), &request)
                .expect("a matching snapshot should remain writable");

        assert!(result.is_none());
    }
}
