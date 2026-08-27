use mangodisk_core::CoreErrorCode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CliExitCode {
    Success = 0,
    Failure = 3,
    ConfirmationRequired = 4,
    OperationBusy = 5,
    PermissionDenied = 6,
    Cancelled = 7,
    CompletedWithWarnings = 8,
}

impl CliExitCode {
    pub const fn from_core_error(code: CoreErrorCode) -> Self {
        match code {
            CoreErrorCode::OperationBusy => Self::OperationBusy,
            CoreErrorCode::OperationCancelled => Self::Cancelled,
            CoreErrorCode::PermissionDenied => Self::PermissionDenied,
            CoreErrorCode::InvalidInput
            | CoreErrorCode::OperationFailed
            | CoreErrorCode::Persistence
            | CoreErrorCode::Platform => Self::Failure,
        }
    }
}

impl From<CliExitCode> for std::process::ExitCode {
    fn from(value: CliExitCode) -> Self {
        Self::from(value as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_codes_map_to_documented_exit_codes() {
        assert_eq!(
            CliExitCode::from_core_error(CoreErrorCode::OperationBusy),
            CliExitCode::OperationBusy
        );
        assert_eq!(
            CliExitCode::from_core_error(CoreErrorCode::OperationCancelled),
            CliExitCode::Cancelled
        );
        assert_eq!(
            CliExitCode::from_core_error(CoreErrorCode::PermissionDenied),
            CliExitCode::PermissionDenied
        );
        assert_eq!(
            CliExitCode::from_core_error(CoreErrorCode::InvalidInput),
            CliExitCode::Failure
        );
    }
}
