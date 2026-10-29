//! The Operator's configuration: `title-select.toml` (ADR 0006).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// How many full Audits a Source Set keeps when the configuration does not say.
pub const DEFAULT_AUDITS_KEPT: usize = 20;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Named Roots: the folders Title Select may read.
    #[serde(default)]
    pub roots: BTreeMap<String, Root>,
    #[serde(default)]
    pub audits: Audits,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Root {
    pub path: PathBuf,
    /// Whether Title Select may write to this Root. Nothing writes yet, so every Root must be read-only.
    #[serde(default)]
    pub writable: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Audits {
    /// The latest full Audits kept per Source Set; older ones are pruned to a summary.
    #[serde(default = "default_keep")]
    pub keep: usize,
}

fn default_keep() -> usize {
    DEFAULT_AUDITS_KEPT
}

impl Default for Audits {
    fn default() -> Self {
        Audits {
            keep: DEFAULT_AUDITS_KEPT,
        }
    }
}

impl Default for Config {
    /// No Roots, and the default Audit history.
    fn default() -> Self {
        Config {
            roots: BTreeMap::new(),
            audits: Audits::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invalid configuration: {0}")]
pub struct Error(String);

impl Config {
    pub fn load(path: &Path) -> Result<Config, Error> {
        let text =
            std::fs::read_to_string(path).map_err(|e| Error(format!("{}: {e}", path.display())))?;
        Config::parse(&text).map_err(|Error(e)| Error(format!("{}: {e}", path.display())))
    }

    pub fn parse(text: &str) -> Result<Config, Error> {
        let config: Config = toml::from_str(text).map_err(|e| Error(e.to_string()))?;
        if config.audits.keep == 0 {
            return Err(Error("audits.keep must be at least 1".into()));
        }
        for (name, root) in &config.roots {
            if name.is_empty() {
                return Err(Error("a Root has an empty name".into()));
            }
            if root.writable {
                return Err(Error(format!(
                    "Root {name} is writable; every Root is read-only until Change Plans exist"
                )));
            }
        }
        Ok(config)
    }

    pub fn root(&self, name: &str) -> Option<&Root> {
        self.roots.get(name)
    }
}
