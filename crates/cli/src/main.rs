mod client;

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use title_select_api::selection::SelectionProfile;
use title_select_engine::{Finding, GameStatus};
use title_select_server::Settings;
use title_select_store::config::Config;
use title_select_store::{ImportOutcome, Store};

/// Audits your source sets against DAT revisions.
#[derive(Parser)]
#[command(version = version())]
struct Cli {
    /// The store directory: the database and the DAT blobs.
    #[arg(long, global = true, env = "TITLE_SELECT_STORE")]
    store: Option<PathBuf>,
    /// The configuration file (`title-select.toml`) that declares your roots.
    #[arg(long, global = true, env = "TITLE_SELECT_CONFIG")]
    config: Option<PathBuf>,
    /// The file holding your operator token: the Service accepts it, and `audit --server` sends it.
    #[arg(long, global = true, env = "TITLE_SELECT_OPERATOR_TOKEN_FILE")]
    token_file: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Audit a source set against a DAT revision, reading every payload, and print a JSON report.
    /// Reads only; never changes the source set.
    ///
    /// With `--dat <file> <dir>` the audit is stateless. With `--set <name>` it audits a registered source set
    /// and records the audit in the store. With `--server <url> --set <name>` the Service at `<url>` runs it as a
    /// job, which this follows to completion, showing progress and a summary on stderr.
    Audit {
        /// The Logiqx XML DAT file to audit against.
        #[arg(long, required_unless_present = "set", conflicts_with = "set")]
        dat: Option<PathBuf>,
        /// The registered source set to audit, by name.
        #[arg(long)]
        set: Option<String>,
        /// With `--set`, the DAT revision to audit against, by a prefix of its SHA-256. Defaults to the latest
        /// audit's DAT revision, else the newest imported revision of the source set's DAT.
        #[arg(long, requires = "set")]
        revision: Option<String>,
        /// The Service to run the audit, such as `http://127.0.0.1:8080`. Sends your operator token.
        #[arg(long, requires = "set")]
        server: Option<String>,
        /// Write the report to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
        /// The source set's directory.
        #[arg(required_unless_present = "set", conflicts_with = "set")]
        source_set: Option<PathBuf>,
    },
    /// Manage DATs and their DAT revisions.
    Dat {
        #[command(subcommand)]
        command: DatCommand,
    },
    /// Manage the source set registry.
    Set {
        #[command(subcommand)]
        command: SetCommand,
    },
    /// Run the Service: the `/api/v1` JSON API and the audit and selection jobs. It holds the store until it stops.
    Serve {
        /// The address to listen on.
        #[arg(long, env = "TITLE_SELECT_LISTEN", default_value = "127.0.0.1:8080")]
        listen: SocketAddr,
        /// The workbench's built bundle (`target/site` after `cargo leptos build`), served at `/`.
        #[arg(long, env = "TITLE_SELECT_UI")]
        ui: Option<PathBuf>,
        /// The selection engine install that `tools/retool/setup` builds, for selections and previews. Defaults
        /// to `/opt/retool`. Without one the Service still runs audits.
        #[arg(long, env = title_select_selection::RETOOL_ENV)]
        retool: Option<PathBuf>,
    },
    /// Select from a DAT with a selection profile, running the pinned selection engine, and print the explained
    /// selection as JSON: every group's winner, alternatives and exclusions with the rule responsible for each.
    ///
    /// The selection engine runs in a scratch folder in the store's `tmp` when `--store` is given, else in the
    /// system's temporary folder, and the folder is deleted afterwards.
    Select {
        /// The Logiqx XML DAT file to select from.
        #[arg(long)]
        dat: PathBuf,
        /// The selection profile, as JSON.
        #[arg(long)]
        profile: PathBuf,
        /// The selection engine install that `tools/retool/setup` builds. Defaults to `/opt/retool`.
        #[arg(long, env = title_select_selection::RETOOL_ENV)]
        retool: Option<PathBuf>,
        /// Seconds the selection engine may run before it is stopped.
        #[arg(long, default_value_t = title_select_selection::DEFAULT_TIMEOUT.as_secs(), value_parser = clap::value_parser!(u64).range(1..))]
        timeout: u64,
        /// Write the selection to this file instead of stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Check that a Service is up, for a container healthcheck. Prints nothing on success; exits 1 with the
    /// reason on stderr otherwise.
    Health {
        /// The Service's health endpoint.
        #[arg(long, default_value = "http://127.0.0.1:8080/api/v1/health")]
        url: String,
        /// Seconds to wait for the whole check, from 1 to 300.
        #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
}

#[derive(Subcommand)]
enum DatCommand {
    /// Import an unmodified DAT file as a DAT revision. Importing the same bytes again changes nothing,
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
    /// Register a folder in a root as a source set, bound to a DAT.
    Add {
        /// The root's name in the configuration.
        root: String,
        /// The source set's folder, relative to the root.
        path: String,
        /// The DAT to audit it against, as `<provider>/<name>`.
        #[arg(long)]
        dat: String,
    },
    /// List the source sets with the DAT revision each was last verified against.
    List,
    /// List a source set's audits, newest first, with time, DAT revision and counts.
    History { name: String },
}

/// The package version, then the upstream commits selection is pinned to.
fn version() -> &'static str {
    static VERSION: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let package = env!("CARGO_PKG_VERSION");
        match title_select_selection::upstream() {
            Ok(up) => format!(
                "{package}\nselection engine: {} at {}, patch set {}\nclone lists and metadata: {} at {}",
                up.retool.repository,
                up.retool.commit,
                up.patch_set_sha256,
                up.clone_lists.repository,
                up.clone_lists.commit,
            ),
            Err(err) => format!("{package}\n{err}"),
        }
    });
    &VERSION
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

/// The configuration file, or no roots when none is given.
fn load_config(path: Option<&Path>) -> Result<Config, String> {
    match path {
        Some(path) => Config::load(path).map_err(|e| e.to_string()),
        None => Ok(Config::default()),
    }
}

fn write_report(report: &impl serde::Serialize, output: Option<PathBuf>) -> Result<(), String> {
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
        Command::Serve { listen, ui, retool } => {
            let store = store.ok_or("no store: pass --store <dir> or set TITLE_SELECT_STORE")?;
            title_select_server::run(Settings {
                store: store.to_path_buf(),
                config: load_config(config)?,
                listen,
                token_file: cli.token_file,
                ui,
                retool,
                hold_jobs: false,
            })
            .map_err(|e| e.to_string())
        }
        Command::Select {
            dat,
            profile,
            retool,
            timeout,
            output,
        } => {
            let profile: SelectionProfile = serde_json::from_slice(
                &std::fs::read(&profile).map_err(|e| format!("{}: {e}", profile.display()))?,
            )
            .map_err(|e| format!("{}: {e}", profile.display()))?;
            let work_dir = match store {
                Some(store) => store.join(title_select_store::TMP),
                None => std::env::temp_dir(),
            };
            let outcome = title_select_selection::Engine::locate(retool.as_deref())
                .and_then(|engine| {
                    engine.run(
                        &dat,
                        &profile,
                        &title_select_selection::RunOptions {
                            work_dir,
                            timeout: std::time::Duration::from_secs(timeout),
                            cancel: None,
                        },
                    )
                })
                .map_err(|e| e.to_string())?;
            for warning in &outcome.warnings {
                eprintln!("title-select: selection engine warning: {warning}");
            }
            write_report(&outcome.selection, output)
        }
        Command::Health { url, timeout } => {
            client::health(&url, std::time::Duration::from_secs(timeout))
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
                "{}: DAT revision {} (version {}) {what}",
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
