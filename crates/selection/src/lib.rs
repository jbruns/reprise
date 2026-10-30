//! The Selection engine (ADR 0001): runs the pinned, trace-patched Retool on one DAT Revision with a Selection
//! Profile, and explains its Selection from the trace.
//!
//! Retool runs as a subprocess from an install that `tools/retool/setup` builds (`.retool/`) or the image ships
//! (`/opt/retool`), in a per-run folder that is deleted afterwards. It never reaches the network: it reads only
//! the pinned clone lists, metadata, MIA and RetroAchievements files beside it.

mod config;
mod trace;

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use title_select_api::selection::{
    SELECTION_TRACE_SCHEMA, Selection, SelectionPins, SelectionProfile,
};

pub use trace::stable_trace;

/// The environment variable that points at a Retool install, for `Engine::locate`.
pub const RETOOL_ENV: &str = "TITLE_SELECT_RETOOL";
/// Where the image installs Retool.
pub const DEFAULT_INSTALL: &str = "/opt/retool";
/// The longest a Selection may run unless the caller says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

const PINS: &str = include_str!("../../../third_party/retool/pins.toml");
/// An unreachable proxy, so that a fetch that escaped the no-network patch fails instead of reaching out.
const NO_PROXY: &str = "http://127.0.0.1:9";
const STDERR_TAIL_LINES: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("selection engine install: {0} (build it with tools/retool/setup)")]
    Install(String),
    #[error("selection profile: {0}")]
    Profile(String),
    #[error("{context}: {source}")]
    Io {
        context: String,
        source: std::io::Error,
    },
    #[error("the selection engine failed ({status}):\n{output}")]
    Retool { status: String, output: String },
    #[error("selection did not finish within {} s", .0.as_secs())]
    Timeout(Duration),
    #[error("the selection engine's trace: {0}")]
    Trace(String),
}

fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Error {
    let context = context.into();
    move |source| Error::Io { context, source }
}

/// How to run one Selection.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The folder the per-run folder is made in, such as the store's `tmp`.
    pub work_dir: PathBuf,
    pub timeout: Duration,
}

/// A Selection and the trace it was derived from.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub selection: Selection,
    /// Retool's trace, as JSON Lines.
    pub trace: String,
}

/// A verified Retool install.
#[derive(Debug, Clone)]
pub struct Engine {
    root: PathBuf,
    data: PathBuf,
    pins: SelectionPins,
}

impl Engine {
    /// The install at `explicit`, else at `$TITLE_SELECT_RETOOL`, else at `/opt/retool`.
    pub fn locate(explicit: Option<&Path>) -> Result<Self, Error> {
        let root = match explicit {
            Some(path) => path.to_path_buf(),
            None => std::env::var_os(RETOOL_ENV)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(DEFAULT_INSTALL)),
        };
        Self::open(&root)
    }

    /// The install at `root`, if it was built from the pins this binary was built with.
    pub fn open(root: &Path) -> Result<Self, Error> {
        // Retool runs in its own folder, so every path it is given must be absolute.
        let root = &std::path::absolute(root).map_err(io(root.display().to_string()))?;
        let expected = expected_pins()?;
        let path = root.join("pins.json");
        let bytes =
            std::fs::read(&path).map_err(|e| Error::Install(format!("{}: {e}", path.display())))?;
        #[derive(serde::Deserialize, PartialEq)]
        struct Installed {
            retool_commit: String,
            patch_set_sha256: String,
            clone_list_commit: String,
        }
        let installed: Installed = serde_json::from_slice(&bytes)
            .map_err(|e| Error::Install(format!("{}: {e}", path.display())))?;
        let wanted = Installed {
            retool_commit: expected.retool_commit.clone(),
            patch_set_sha256: expected.patch_set_sha256.clone(),
            clone_list_commit: expected.clone_list_commit.clone(),
        };
        if installed != wanted {
            return Err(Error::Install(format!(
                "{} was built from other pins than third_party/retool/pins.toml",
                root.display()
            )));
        }
        for required in [
            "venv",
            "retool/retool.py",
            "retool/config/internal-config.json",
        ] {
            if !root.join(required).exists() {
                return Err(Error::Install(format!(
                    "{} has no {required}",
                    root.display()
                )));
            }
        }
        Ok(Engine {
            root: root.to_path_buf(),
            data: root.join("retool"),
            pins: expected,
        })
    }

    /// Reads clone lists, metadata, MIAs and RetroAchievements from `dir` instead of the pinned ones. Tests use
    /// it to run Retool's own fixtures.
    pub fn with_data_dir(mut self, dir: &Path) -> Self {
        self.data = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
        self
    }

    pub fn pins(&self) -> &SelectionPins {
        &self.pins
    }

    /// The pristine, unpatched Retool beside the patched one, when the install has it (`tools/retool/setup`
    /// builds it; the image does not ship it).
    pub fn pristine(&self) -> Option<PathBuf> {
        let path = self.root.join("pristine");
        path.join("retool.py").is_file().then_some(path)
    }

    /// The install's Python.
    pub fn python(&self) -> PathBuf {
        self.root.join("venv").join("bin").join("python")
    }

    pub fn select(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
    ) -> Result<Selection, Error> {
        self.run(dat, profile, options)
            .map(|outcome| outcome.selection)
    }

    /// Selects from `dat` with `profile`, and keeps the trace.
    pub fn run(
        &self,
        dat: &Path,
        profile: &SelectionProfile,
        options: &RunOptions,
    ) -> Result<Outcome, Error> {
        let retool = self.root.join("retool");
        let internal = retool.join("config").join("internal-config.json");
        let vocabulary = config::Vocabulary::parse(
            &std::fs::read(&internal).map_err(io(internal.display().to_string()))?,
        )?;
        vocabulary.check(profile)?;
        let dat = std::path::absolute(dat).map_err(io(dat.display().to_string()))?;
        let dat_sha256 = sha256_file(&dat)?;

        let work_dir = std::path::absolute(&options.work_dir)
            .map_err(io(options.work_dir.display().to_string()))?;
        std::fs::create_dir_all(&work_dir).map_err(io(work_dir.display().to_string()))?;
        let run = tempfile::Builder::new()
            .prefix("selection-")
            .tempdir_in(&work_dir)
            .map_err(io(work_dir.display().to_string()))?;
        let run_dir = run.path();
        let write = |path: PathBuf, contents: String| {
            std::fs::write(&path, contents).map_err(io(path.display().to_string()))
        };
        write(
            run_dir.join("user-config.yaml"),
            config::user_config(profile, &vocabulary),
        )?;
        let systems = run_dir.join("systems");
        std::fs::create_dir(&systems).map_err(io(systems.display().to_string()))?;
        for (name, system) in &profile.systems {
            write(
                systems.join(format!("{name}.yaml")),
                config::system_config(system),
            )?;
        }
        let trace_path = run_dir.join("trace.jsonl");
        let stdout_path = run_dir.join("stdout.txt");
        let stderr_path = run_dir.join("stderr.txt");

        let mut command = Command::new(self.python());
        command
            .arg(retool.join("retool.py"))
            .arg(&dat)
            .arg("--config")
            .arg(run_dir.join("user-config.yaml"))
            .arg("--clonelist-dir")
            .arg(self.data.join("clonelists"))
            .arg("--metadata-dir")
            .arg(self.data.join("metadata"))
            .arg("--mia-dir")
            .arg(self.data.join("mias"))
            .arg("--ra-dir")
            .arg(self.data.join("retroachievements"))
            .arg("--system-config-dir")
            .arg(&systems)
            .arg("--output")
            .arg(run_dir.join("out"))
            .arg("--trace-json")
            .arg(&trace_path)
            .args(["--singlecpu", "--reprocess", "--labelmia", "--labelretro"])
            .args(config::flags(profile))
            .current_dir(run_dir)
            .env_clear()
            .envs([
                ("PYTHONHASHSEED", "0"),
                ("PYTHONDONTWRITEBYTECODE", "1"),
                ("PYTHONIOENCODING", "utf-8"),
                ("LANG", "C.UTF-8"),
                ("http_proxy", NO_PROXY),
                ("https_proxy", NO_PROXY),
                ("HTTP_PROXY", NO_PROXY),
                ("HTTPS_PROXY", NO_PROXY),
            ])
            .env("HOME", run_dir)
            .stdin(Stdio::null())
            .stdout(File::create(&stdout_path).map_err(io(stdout_path.display().to_string()))?)
            .stderr(File::create(&stderr_path).map_err(io(stderr_path.display().to_string()))?);
        let mut child = command
            .spawn()
            .map_err(io(format!("starting {}", self.python().display())))?;
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child
                .try_wait()
                .map_err(io("waiting for the selection engine"))?
            {
                break status;
            }
            if started.elapsed() >= options.timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout(options.timeout));
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        if !status.success() {
            let tail = |path: &Path| {
                let text = std::fs::read_to_string(path).unwrap_or_default();
                let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
                lines[lines.len().saturating_sub(STDERR_TAIL_LINES)..].join("\n")
            };
            let mut output = tail(&stderr_path);
            if output.is_empty() {
                output = tail(&stdout_path);
            }
            return Err(Error::Retool {
                status: status.to_string(),
                output,
            });
        }
        let trace =
            std::fs::read_to_string(&trace_path).map_err(io(trace_path.display().to_string()))?;
        let derived = trace::derive(&trace)?;
        run.close().map_err(io("removing the run folder"))?;
        Ok(Outcome {
            selection: Selection {
                pins: self.pins.clone(),
                dat_sha256,
                system_settings: profile.systems.contains_key(&derived.system),
                system: derived.system,
                summary: derived.summary,
                coverage_drift: derived.coverage_drift,
                groups: derived.groups,
            },
            trace,
        })
    }
}

/// The pins this binary was built with: those in `third_party/retool/pins.toml`.
fn expected_pins() -> Result<SelectionPins, Error> {
    #[derive(serde::Deserialize)]
    struct Pins {
        retool: Commit,
        patches: Vec<Patch>,
        clone_lists: Commit,
    }
    #[derive(serde::Deserialize)]
    struct Commit {
        commit: String,
    }
    #[derive(serde::Deserialize)]
    struct Patch {
        sha256: String,
    }
    let pins: Pins = toml::from_str(PINS).map_err(|e| Error::Install(format!("pins.toml: {e}")))?;
    let concatenated: String = pins.patches.iter().map(|p| p.sha256.as_str()).collect();
    Ok(SelectionPins {
        retool_commit: pins.retool.commit,
        patch_set_sha256: hex(&Sha256::digest(concatenated.as_bytes())),
        clone_list_commit: pins.clone_lists.commit,
        trace_schema: SELECTION_TRACE_SCHEMA,
    })
}

fn sha256_file(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path).map_err(io(path.display().to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1 << 16];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(io(path.display().to_string()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
