//! Find a Cargo package and invoke `cargo run` / `cargo test`.

use crate::error::CliError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Commands that `AppCli` in the application binary understands.
pub const APP_COMMANDS: &[&str] = &[
    "run",
    "routes",
    "check",
    "dbshell",
    "makemigrations",
    "migrate",
    "rollback",
    "showmigrations",
    "inspectmigrations",
    "squashmigrations",
];

/// Whether `command` is forwarded to the application binary.
pub fn is_app_command(command: &str) -> bool {
    APP_COMMANDS.contains(&command)
}

/// Walk up from `start` to a `Cargo.toml` that defines a `[package]`.
pub fn find_app_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let manifest = dir.join("Cargo.toml");
        if manifest.is_file()
            && let Ok(text) = std::fs::read_to_string(&manifest)
            && text.contains("[package]")
        {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// `cargo run -- args` in `package_dir`. Forwards the child's exit code.
pub fn cargo_run(package_dir: &Path, args: &[String]) -> Result<u8, CliError> {
    cargo(package_dir, "run", args)
}

/// Cargo subcommands `siderite` forwards verbatim in the app package.
pub const CARGO_COMMANDS: &[&str] = &["build", "test"];

/// `cargo <cargo_cmd>` in `package_dir` with the args after `cargo_cmd`
/// (no extra `--` unless the user passed some).
pub fn cargo_passthrough(
    package_dir: &Path,
    cargo_cmd: &str,
    args: &[String],
) -> Result<u8, CliError> {
    cargo(package_dir, cargo_cmd, &args_after(cargo_cmd, args))
}

fn args_after(cargo_cmd: &str, args: &[String]) -> Vec<String> {
    args.iter()
        .skip_while(|a| a.as_str() != cargo_cmd)
        .skip(1)
        .cloned()
        .collect()
}

fn cargo(package_dir: &Path, cargo_cmd: &str, args: &[String]) -> Result<u8, CliError> {
    let mut command = Command::new("cargo");
    command.arg(cargo_cmd).current_dir(package_dir);
    if cargo_cmd == "run" {
        command.arg("--").args(args);
    } else {
        command.args(args);
    }
    let status = command.status().map_err(|err| {
        CliError::Io(format!(
            "cannot run cargo {cargo_cmd} (is cargo on PATH?): {err}"
        ))
    })?;
    Ok(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_package_and_skips_a_workspace_root() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(find_app_dir(&root).as_deref(), Some(root.as_path()));
        let workspace = root.parent().unwrap().parent().unwrap();
        let found = find_app_dir(workspace);
        assert!(
            found.as_deref() != Some(workspace),
            "workspace root has no [package]: {found:?}"
        );
    }

    #[test]
    fn passthrough_forwards_args_after_the_command() {
        let args: Vec<String> = ["build", "--release", "--features", "x"]
            .iter()
            .map(|a| (*a).to_owned())
            .collect();
        assert_eq!(args_after("build", &args), args[1..].to_vec());
        assert!(args_after("test", &args[..1]).is_empty());
    }
}
