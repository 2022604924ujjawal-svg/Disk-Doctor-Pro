mod arguments;
mod commands;
mod exit_code;
mod output;
mod progress;

use clap::Parser;
use mangodisk_core::{configure_application_paths, ApplicationPaths, APPLICATION_IDENTIFIER};
use mangodisk_platform::application_directories;

use std::io::{self, IsTerminal};

use arguments::{Cli, ColorMode, OutputFormat};
use commands::CommandContext;
use exit_code::CliExitCode;
use output::{write_error, write_outcome};
use progress::CancellationController;

fn main() -> std::process::ExitCode {
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("MANGODISK_LOG", "error"))
        .target(env_logger::Target::Stderr)
        .format_timestamp_millis()
        .init();

    let cli = Cli::parse();
    if let Err(error) = configure_storage() {
        write_error(&error, cli.format, CliExitCode::Failure);
        return CliExitCode::Failure.into();
    }
    let cancellation = match CancellationController::install() {
        Ok(controller) => controller,
        Err(error) => {
            write_error(&error, cli.format, CliExitCode::Failure);
            return CliExitCode::Failure.into();
        }
    };
    let context = CommandContext {
        format: cli.format,
        color_enabled: color_enabled(cli.color, cli.format),
        include_full_paths: cli.include_full_paths,
        progress_enabled: !cli.no_progress,
        cancellation: &cancellation,
    };
    match commands::dispatch(cli.command, &context) {
        Ok(outcome) => match write_outcome(outcome, cli.format, cli.include_full_paths) {
            Ok(code) => code.into(),
            Err(error) => {
                write_error(&error, cli.format, CliExitCode::Failure);
                CliExitCode::Failure.into()
            }
        },
        Err(error) => {
            write_error(&error.message, cli.format, error.exit_code);
            error.exit_code.into()
        }
    }
}

fn configure_storage() -> Result<(), String> {
    let directories =
        application_directories(APPLICATION_IDENTIFIER).map_err(|error| error.to_string())?;
    let paths = ApplicationPaths::from_base_directories(
        directories.local_data_directory,
        directories.cache_directory,
    )
    .map_err(|error| error.to_string())?;
    configure_application_paths(paths).map_err(|error| error.to_string())
}

fn color_enabled(mode: ColorMode, format: OutputFormat) -> bool {
    if format != OutputFormat::Human || std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => io::stdout().is_terminal(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_output_never_uses_terminal_colors() {
        assert!(!color_enabled(ColorMode::Always, OutputFormat::Json));
        assert!(!color_enabled(ColorMode::Always, OutputFormat::Jsonl));
    }
}
