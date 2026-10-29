use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Audits Verified Sets against DAT Revisions.
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Audit a Verified Set against a DAT Revision, reading every payload, and print a JSON report.
    /// Reads only; never changes the Verified Set.
    Audit {
        /// The Logiqx XML DAT file to audit against.
        #[arg(long)]
        dat: PathBuf,
        /// Write the report to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// The Verified Set's directory.
        verified_set: PathBuf,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("title-select: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Audit {
            dat,
            output,
            verified_set,
        } => {
            let report = title_select::audit(&dat, &verified_set).map_err(|e| e.to_string())?;
            let mut json = serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?;
            json.push(b'\n');
            match output {
                Some(path) => {
                    std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display()))
                }
                None => std::io::stdout()
                    .write_all(&json)
                    .map_err(|e| e.to_string()),
            }
        }
    }
}
