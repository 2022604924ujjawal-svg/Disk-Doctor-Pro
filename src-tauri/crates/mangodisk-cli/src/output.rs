use std::path::Path;

use mangodisk_core::diagnostic_path;
use serde::Serialize;
use serde_json::{json, Value};

use crate::{arguments::OutputFormat, exit_code::CliExitCode};

pub const OUTPUT_SCHEMA_VERSION: &str = "1.0";

#[derive(Debug)]
pub struct CommandOutcome {
    pub command: &'static str,
    pub human: String,
    pub data: Value,
    pub exit_code: CliExitCode,
}

impl CommandOutcome {
    pub fn success(
        command: &'static str,
        human: impl Into<String>,
        data: impl Serialize,
    ) -> Result<Self, String> {
        Ok(Self {
            command,
            human: human.into(),
            data: serde_json::to_value(data)
                .map_err(|error| format!("failed to serialize CLI result: {error}"))?,
            exit_code: CliExitCode::Success,
        })
    }
}

pub fn write_outcome(
    outcome: CommandOutcome,
    format: OutputFormat,
    include_full_paths: bool,
) -> Result<CliExitCode, String> {
    let data = if include_full_paths {
        outcome.data
    } else {
        redact_paths(outcome.data)
    };
    match format {
        OutputFormat::Human => println!("{}", outcome.human),
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schemaVersion": OUTPUT_SCHEMA_VERSION,
                "command": outcome.command,
                "data": data,
            }))
            .map_err(|error| format!("failed to serialize JSON output: {error}"))?
        ),
        OutputFormat::Jsonl => println!(
            "{}",
            serde_json::to_string(&json!({
                "schemaVersion": OUTPUT_SCHEMA_VERSION,
                "type": "result",
                "command": outcome.command,
                "data": data,
            }))
            .map_err(|error| format!("failed to serialize JSONL output: {error}"))?
        ),
    }
    Ok(outcome.exit_code)
}

pub fn write_error(message: &str, format: OutputFormat, code: CliExitCode) {
    match format {
        OutputFormat::Human => eprintln!("MangoDisk CLI failed: {message}"),
        OutputFormat::Json => println!(
            "{}",
            json!({
                "schemaVersion": OUTPUT_SCHEMA_VERSION,
                "error": { "code": code as u8, "message": message },
            })
        ),
        OutputFormat::Jsonl => println!(
            "{}",
            json!({
                "schemaVersion": OUTPUT_SCHEMA_VERSION,
                "type": "error",
                "error": { "code": code as u8, "message": message },
            })
        ),
    }
}

fn redact_paths(mut value: Value) -> Value {
    redact_value(&mut value, None);
    value
}

fn redact_value(value: &mut Value, key: Option<&str>) {
    match value {
        Value::Object(object) => {
            for (child_key, child) in object {
                redact_value(child, Some(child_key));
            }
        }
        Value::Array(values) => {
            for child in values {
                redact_value(child, key);
            }
        }
        Value::String(path) if key.is_some_and(is_path_key) => {
            *path = diagnostic_path(Path::new(path));
        }
        _ => {}
    }
}

fn is_path_key(key: &str) -> bool {
    matches!(
        key,
        "path"
            | "root"
            | "roots"
            | "mountPoint"
            | "parentPath"
            | "currentPath"
            | "removedPath"
            | "removedPaths"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_output_redacts_nested_paths_by_default() {
        let source = json!({
            "root": "/Users/example/private",
            "items": [{"currentPath": "/Users/example/private/file.bin"}],
            "name": "/Users/example/not-a-path-field"
        });

        let redacted = redact_paths(source);

        assert_ne!(redacted["root"], "/Users/example/private");
        assert_ne!(
            redacted["items"][0]["currentPath"],
            "/Users/example/private/file.bin"
        );
        assert_eq!(redacted["name"], "/Users/example/not-a-path-field");
    }
}
