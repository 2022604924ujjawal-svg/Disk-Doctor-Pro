mod clean;

use crate::{
    arguments::{Command, OutputFormat},
    exit_code::CliExitCode,
    output::CommandOutcome,
    progress::CancellationController,
};
use mangodisk_core::CoreError;

pub struct CommandContext<'a> {
    pub format: OutputFormat,
    pub color_enabled: bool,
    pub include_full_paths: bool,
    pub progress_enabled: bool,
    pub cancellation: &'a CancellationController,
}

#[derive(Debug)]
pub struct CliFailure {
    pub message: String,
    pub exit_code: CliExitCode,
}

impl CliFailure {
    pub fn confirmation_required(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            exit_code: CliExitCode::ConfirmationRequired,
        }
    }
}

impl From<String> for CliFailure {
    fn from(message: String) -> Self {
        Self {
            exit_code: CliExitCode::Failure,
            message,
        }
    }
}

impl From<std::io::Error> for CliFailure {
    fn from(error: std::io::Error) -> Self {
        let exit_code = if error.kind() == std::io::ErrorKind::PermissionDenied {
            CliExitCode::PermissionDenied
        } else {
            CliExitCode::Failure
        };
        Self {
            message: error.to_string(),
            exit_code,
        }
    }
}

impl From<CoreError> for CliFailure {
    fn from(error: CoreError) -> Self {
        Self {
            exit_code: CliExitCode::from_core_error(error.code()),
            message: error.to_string(),
        }
    }
}

pub fn dispatch(
    command: Command,
    context: &CommandContext<'_>,
) -> Result<CommandOutcome, CliFailure> {
    match command {
        Command::Clean(arguments) => clean::run(arguments, context),
    }
}
