//! `axumapi` CLI: apply JSON migrations, squash them, and show their status.

#![forbid(unsafe_code)]

use std::process::ExitCode as ProcessExit;

#[tokio::main]
async fn main() -> ProcessExit {
    match axumapi_cli::run().await {
        Ok(code) => ProcessExit::from(code.0),
        Err(err) => {
            eprintln!("{err}");
            ProcessExit::from(1)
        }
    }
}
