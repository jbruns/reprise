//! Running the selection engine for the Service: Selection Jobs and live previews, each on a copy of the DAT
//! Revision's blob named as it was imported, in a scratch folder under the store's `tmp`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use title_select_api::selection::{Selection, SelectionProfile};
use title_select_selection::{Engine, Error, RunOptions};
use title_select_store::{Store, TMP};

/// Why a Selection or preview cannot run without an engine install.
pub const UNAVAILABLE: &str = "the selection engine is not installed on this server, so selections and previews \
    are unavailable; start the server with --retool, or set TITLE_SELECT_RETOOL";

/// Runs the engine on the DAT blob `blob`, staged under its imported file name `file`.
pub fn select(
    store: &Store,
    engine: &Engine,
    blob: &Path,
    file: &str,
    profile: &SelectionProfile,
    cancel: Option<Arc<AtomicBool>>,
) -> Result<Selection, Error> {
    let work = store.dir().join(TMP);
    let io = |context: String| move |source| Error::Io { context, source };
    std::fs::create_dir_all(&work).map_err(io(work.display().to_string()))?;
    let staged = tempfile::Builder::new()
        .prefix("dat-")
        .tempdir_in(&work)
        .map_err(io(work.display().to_string()))?;
    let name = Path::new(file)
        .file_name()
        .filter(|n| !n.is_empty())
        .unwrap_or(std::ffi::OsStr::new("input.dat"));
    let dat = staged.path().join(name);
    if std::fs::hard_link(blob, &dat).is_err() {
        std::fs::copy(blob, &dat).map_err(io(blob.display().to_string()))?;
    }
    let mut options = RunOptions::new(&work);
    options.cancel = cancel;
    let selection = engine.run(&dat, profile, &options)?.selection;
    drop(staged);
    Ok(selection)
}

/// The preview each actor has in flight. A newer preview from the same actor cancels the older one.
#[derive(Default)]
pub struct Previews(Mutex<HashMap<String, Arc<AtomicBool>>>);

impl Previews {
    /// Registers a new preview for `actor`, cancelling the one in flight, if any.
    pub fn start(self: &Arc<Self>, actor: &str) -> PreviewGuard {
        let flag = Arc::new(AtomicBool::new(false));
        if let Some(older) = self
            .0
            .lock()
            .unwrap()
            .insert(actor.to_owned(), flag.clone())
        {
            older.store(true, Ordering::Relaxed);
        }
        PreviewGuard {
            previews: self.clone(),
            actor: actor.to_owned(),
            flag,
        }
    }
}

/// Cancels its preview when dropped: when the preview ends, or when the client went away and the request was
/// dropped.
pub struct PreviewGuard {
    previews: Arc<Previews>,
    actor: String,
    flag: Arc<AtomicBool>,
}

impl PreviewGuard {
    pub fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

impl Drop for PreviewGuard {
    fn drop(&mut self) {
        self.flag.store(true, Ordering::Relaxed);
        let mut previews = self.previews.0.lock().unwrap();
        if previews
            .get(&self.actor)
            .is_some_and(|f| Arc::ptr_eq(f, &self.flag))
        {
            previews.remove(&self.actor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_preview_cancels_the_older_one_of_the_same_actor_only() {
        let previews = Arc::new(Previews::default());
        let older = previews.start("someone");
        let other = previews.start("someone else");
        let newer = previews.start("someone");

        assert!(older.flag().load(Ordering::Relaxed));
        assert!(!newer.flag().load(Ordering::Relaxed));
        assert!(!other.flag().load(Ordering::Relaxed));
        drop(older);
        assert!(
            previews.0.lock().unwrap().contains_key("someone"),
            "the older guard leaves the newer preview registered"
        );
        let flag = newer.flag();
        drop(newer);
        assert!(
            flag.load(Ordering::Relaxed),
            "a dropped request kills its run"
        );
        assert!(!previews.0.lock().unwrap().contains_key("someone"));
    }
}
