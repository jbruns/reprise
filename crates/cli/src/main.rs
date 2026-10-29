mod client;

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use title_select_engine::{Finding, GameStatus, Report};
use title_select_server::Settings;
use title_select_store::config::Config;
use title_select_store::{ImportOutcome, Store};

/// Audits Source Sets against DAT Revisions.
#[derive(Parser)]
#[command(version)]
struct Cli {
    /// The store directory: the database and the DAT blobs.
    #[arg(long, global = true, env = "TITLE_SELECT_STORE")]
    store: Option<PathBuf>,
    /// The configuration file (`title-select.toml`) that declares the Roots.
    #[arg(long, global = true, env = "TITLE_SELECT_CONFIG")]
    config: Option<PathBuf>,
    /// The file holding the Operator token: the Service accepts it, and `audit --server` sends it.
    #[arg(long, global = true, env = "TITLE_SELECT_OPERATOR_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Audit a Source Set against a DAT Revision, reading every payload, and print a JSON report.
    /// Reads only; never changes the Source Set.
    ///
    /// With `--dat <file> <dir>` the Audit is stateless. With `--set <name>` it audits a registered Source Set
    /// and records the Audit in the store. With `--server <url> --set <name>` the Service at `<url>` runs it as a
    /// Job, which this follows to completion, showing progress and a summary on stderr.
    Audit {
        /// The Logiqx XML DAT file to audit against.
        #[arg(long, required_unless_present = "set", conflicts_with = "set")]
        dat: Option<PathBuf>,
        /// The registered Source Set to audit, by name.
        #[arg(long)]
        set: Option<String>,
        /// With `--set`, the DAT Revision to audit against, by a prefix of its SHA-256. Defaults to the latest
        /// Audit's DAT Revision, else the newest imported revision of the Source Set's DAT.
        #[arg(long, requires = "set")]
        revision: Option<String>,
        /// The Service to run the Audit, such as `http://127.0.0.1:8080`. Sends the Operator token.
        #[arg(long, requires = "set")]
        server: Option<String>,
        /// Write the report to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// The Source Set's directory.
        #[arg(required_unless_present = "set", conflicts_with = "set")]
        source_set: Option<PathBuf>,
    },
    /// Manage DATs and their DAT Revisions.
    Dat {
        #[command(subcommand)]
        command: DatCommand,
    },
    /// Manage the Source Set registry.
    Set {
        #[command(subcommand)]
        command: SetCommand,
    },
    /// Run the Service: the `/api/v1` JSON API and the Audit Jobs. It holds the store until it stops.
    Serve {
        /// The address to listen on.
        #[arg(long, env = "TITLE_SELECT_LISTEN", default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// The workbench's built bundle (`target/site` after `cargo leptos build`), served at `/`.
        #[arg(long, env = "TITLE_SELECT_UI")]
        ui: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
enum DatCommand {
    /// Import an unmodified DAT file as a DAT Revision. Importing the same bytes again changes nothing,
    /// except to restore the file if the store had dropped it.
    Import {
        file: PathBuf,
        /// Who publishes the DAT, for example `No-Intro`.
        #[arg(long)]
        provider: String,
        /// Where the file was downloaded from.
        #[arg(long)]
        source_url: Option<String>,
    },
}

#[derive(Subcommand)]
enum SetCommand {
    /// Register a folder in a Root as a Source Set, bound to a DAT.
    Add {
        /// The Root's name in the configuration.
        root: String,
        /// The Source Set's folder, relative to the Root.
        path: String,
        /// The DAT to audit it against, as `<provider>/<name>`.
        #[arg(long)]
        dat: String,
    },
    /// List the Source Sets with the DAT Revision each was last verified against.
    List,
    /// List a Source Set's Audits, newest first, with time, DAT Revision and counts.
    History { name: String },
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

fn open_store(cli_store: Option<&Path>) -> Result<Store, String> {
    let dir = cli_store.ok_or("no store: pass --store <dir> or set TITLE_SELECT_STORE")?;
    Store::open(dir).map_err(|e| e.to_string())
}

/// The configuration file, or no Roots when none is given.
fn load_config(path: Option<&Path>) -> Result<Config, String> {
    match path {
        Some(path) => Config::load(path).map_err(|e| e.to_string()),
        None => Ok(Config::default()),
    }
}

fn write_report(report: &Report, output: Option<PathBuf>) -> Result<(), String> {
    let mut json = serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?;
    json.push(b'\n');
    match output {
        Some(path) => std::fs::write(&path, json).map_err(|e| format!("{}: {e}", path.display())),
        None => std::io::stdout()
            .write_all(&json)
            .map_err(|e| e.to_string()),
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let store = cli.store.as_deref();
    let config = cli.config.as_deref();
    match cli.command {
        Command::Audit {
            set: Some(set),
            revision,
            server: Some(server),
            output,
            ..
        } => {
            let report = client::audit(client::Request {
                server: &server,
                set,
                revision,
                token_file: cli.token_file.as_deref(),
            })?;
            write_report(&report, output)
        }
        Command::Audit {
            dat: Some(dat),
            source_set: Some(source_set),
            output,
            ..
        } => {
            let report =
                title_select_engine::audit(&dat, &source_set).map_err(|e| e.to_string())?;
            write_report(&report, output)
        }
        Command::Audit {
            set: Some(set),
            revision,
            output,
            ..
        } => {
            let config = load_config(config)?;
            let audit = open_store(store)?
                .audit_source_set(&config, &set, revision.as_deref())
                .map_err(|e| e.to_string())?;
            write_report(&audit.report, output)
        }
        Command::Audit { .. } => unreachable!("clap requires --dat with a directory, or --set"),
        Command::Serve { listen, ui } => {
            let store = store.ok_or("no store: pass --store <dir> or set TITLE_SELECT_STORE")?;
            title_select_server::run(Settings {
                store: store.to_path_buf(),
                config: load_config(config)?,
                listen,
                token_file: cli.token_file,
                ui,
                hold_jobs: false,
            })
            .map_err(|e| e.to_string())
        }
        Command::Dat {
            command:
                DatCommand::Import {
                    file,
                    provider,
                    source_url,
                },
        } => {
            let (revision, outcome) = open_store(store)?
                .import_dat(&file, &provider, source_url.as_deref())
                .map_err(|e| e.to_string())?;
            let what = match outcome {
                ImportOutcome::Added => "imported",
                ImportOutcome::AlreadyImported => "already imported",
                ImportOutcome::Restored => "already imported; its file is restored",
            };
            println!(
                "{}: DAT Revision {} (version {}) {what}",
                revision.dat(),
                revision.sha256,
                revision.version
            );
            Ok(())
        }
        Command::Set {
            command: SetCommand::Add { root, path, dat },
        } => {
            let config = load_config(config)?;
            let set = open_store(store)?
                .add_source_set(&config, &root, &path, &dat)
                .map_err(|e| e.to_string())?;
            println!(
                "{}: added from {}:{}, bound to {}",
                set.name, set.root, set.path, set.dat
            );
            Ok(())
        }
        Command::Set {
            command: SetCommand::List,
        } => {
            for set in open_store(store)?
                .source_sets()
                .map_err(|e| e.to_string())?
            {
                let last = match &set.last_verified_against {
                    Some(l) => format!("last verified against {} ({})", l.sha256, l.version),
                    None => "never audited".to_owned(),
                };
                println!(
                    "{}\t{}:{}\t{}\t{last}",
                    set.name, set.root, set.path, set.dat
                );
            }
            Ok(())
        }
        Command::Set {
            command: SetCommand::History { name },
        } => {
            for audit in open_store(store)?
                .history(&name)
                .map_err(|e| e.to_string())?
            {
                let findings = Finding::ALL
                    .iter()
                    .map(|f| format!("{}={}", f.as_str(), audit.findings[f]));
                let games = GameStatus::ALL
                    .iter()
                    .map(|g| format!("{}={}", g.as_str(), audit.games[g]));
                let counts: Vec<String> = findings.chain(games).collect();
                println!(
                    "{}\t{}\t{} ({})\t{}\t{}",
                    audit.id,
                    audit.finished_at,
                    audit.dat_revision_sha256,
                    audit.dat_revision_version,
                    counts.join(" "),
                    if audit.pruned { "summary" } else { "full" }
                );
            }
            Ok(())
        }
    }
}
