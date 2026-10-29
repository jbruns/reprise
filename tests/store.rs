mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use title_select::config::Config;
use title_select::store::{Error, ImportOutcome, Store};

const SET: &str = "No-Intro/Test - System";

/// A store, a Root called `sets` holding one Source Set folder, and a folder of DAT files to import.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
}

impl Env {
    fn new(keep: usize) -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("roots/sets");
        std::fs::create_dir_all(root.join(SET)).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n\n[audits]\nkeep = {keep}\n",
            root.display().to_string()
        ))
        .unwrap();
        let store = Store::open(&tmp.path().join("store")).unwrap();
        Env { tmp, store, config }
    }

    fn root(&self) -> PathBuf {
        self.tmp.path().join("roots/sets")
    }

    fn set_dir(&self) -> PathBuf {
        self.root().join(SET)
    }

    /// Revision 1 lists Pong and Combat; revision 2 adds Breakout.
    fn dat(&self, revision: u32) -> PathBuf {
        let mut games = vec![
            ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
            ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ];
        if revision >= 2 {
            games.push(("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]));
        }
        write_dat_version(
            &self.tmp.path().join("in"),
            &format!("Test - System ({revision}).dat"),
            &format!("2026010{revision}-000000"),
            &games,
        )
    }

    fn import(&self, revision: u32) -> (String, ImportOutcome) {
        let (rev, outcome) = self
            .store
            .import_dat(&self.dat(revision), "No-Intro", None)
            .unwrap();
        (rev.sha256, outcome)
    }

    fn add_set(&self) {
        self.store
            .add_source_set(&self.config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
    }

    fn audit(&self, revision: Option<&str>) -> title_select::store::StoredAudit {
        self.store
            .audit_source_set(&self.config, "Test - System", revision)
            .unwrap()
    }

    fn blob_present(&self, sha256: &str) -> bool {
        let row = self
            .store
            .dat_revisions()
            .unwrap()
            .into_iter()
            .find(|r| r.sha256 == sha256)
            .unwrap();
        assert_eq!(row.blob_present, self.store.blob_path(sha256).is_file());
        row.blob_present
    }
}

#[test]
fn importing_the_same_bytes_again_returns_the_existing_dat_revision() {
    let env = Env::new(20);
    let path = env.dat(1);
    let (first, outcome) = env
        .store
        .import_dat(&path, "No-Intro", Some("https://<domain>/dat"))
        .unwrap();
    assert_eq!(outcome, ImportOutcome::Added);
    assert_eq!(first.name, "Test - System");
    assert_eq!(first.version, "20260101-000000");
    assert_eq!(first.file, "Test - System (1).dat");
    assert_eq!(first.source_url.as_deref(), Some("https://<domain>/dat"));

    let (again, outcome) = env.store.import_dat(&path, "No-Intro", None).unwrap();

    assert_eq!(outcome, ImportOutcome::AlreadyImported);
    assert_eq!(again, first);
    assert_eq!(env.store.dat_revisions().unwrap().len(), 1);
    assert_eq!(
        std::fs::read(env.store.blob_path(&first.sha256)).unwrap(),
        std::fs::read(&path).unwrap()
    );
}

#[test]
fn an_unheld_blob_is_dropped_and_importing_its_bytes_again_restores_it() {
    let env = Env::new(20);
    let (old, _) = env.import(1);
    let (new, _) = env.import(2);

    assert!(!env.blob_present(&old), "nothing holds the older revision");
    assert!(
        env.blob_present(&new),
        "the newest revision of a DAT is held"
    );
    assert_eq!(env.store.dat_revisions().unwrap().len(), 2, "the row stays");

    let (restored, outcome) = env.import(1);

    assert_eq!(outcome, ImportOutcome::Restored);
    assert_eq!(restored, old);
    assert!(env.blob_present(&old));
    assert_eq!(
        std::fs::read(env.store.blob_path(&old)).unwrap(),
        std::fs::read(env.dat(1)).unwrap()
    );
}

#[test]
fn a_blob_without_a_row_is_collected() {
    let env = Env::new(20);
    let (held, _) = env.import(1);
    let dats = env.store.dir().join("dats");
    let orphan = format!("{:064x}", 1);
    std::fs::write(dats.join(&orphan), b"no row").unwrap();
    std::fs::write(dats.join(format!(".{orphan}.tmp")), b"interrupted").unwrap();

    let removed = env.store.collect_garbage().unwrap();

    assert_eq!(removed, vec![format!(".{orphan}.tmp"), orphan.clone()]);
    assert!(env.store.blob_path(&held).is_file());

    std::fs::write(dats.join(&orphan), b"no row").unwrap();
    drop(Store::open(env.store.dir()).unwrap());
    assert!(
        !dats.join(&orphan).exists(),
        "opening the store collects too"
    );
}

#[test]
fn a_source_set_path_that_escapes_its_root_is_refused() {
    let env = Env::new(20);
    env.import(1);
    let outside = env.tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, env.root().join("escape")).unwrap();
    std::os::unix::fs::symlink(env.set_dir(), env.root().join("alias")).unwrap();

    for rel in [
        "../outside",
        "No-Intro/../../outside",
        outside.to_str().unwrap(),
        "/No-Intro",
        "escape",
        "",
    ] {
        let refused = env
            .store
            .add_source_set(&env.config, "sets", rel, "No-Intro/Test - System");
        assert!(
            matches!(refused, Err(Error::Refused(_))),
            "{rel:?}: {refused:?}"
        );
    }
    assert!(env.store.source_sets().unwrap().is_empty());

    let inside = env
        .store
        .add_source_set(&env.config, "sets", "alias", "No-Intro/Test - System")
        .unwrap();
    assert_eq!(inside.name, "alias");
}

#[test]
fn a_source_set_is_named_by_its_folder_and_bound_to_its_dat() {
    let env = Env::new(20);
    env.import(1);

    env.add_set();

    let sets = env.store.source_sets().unwrap();
    assert_eq!(sets.len(), 1);
    assert_eq!(sets[0].name, "Test - System");
    assert_eq!(sets[0].root, "sets");
    assert_eq!(sets[0].path, SET);
    assert_eq!(sets[0].dat, "No-Intro/Test - System");
    assert_eq!(sets[0].last_verified_against, None);
    let unknown_dat = env
        .store
        .add_source_set(&env.config, "sets", "No-Intro", "No-Intro/Other");
    assert!(matches!(unknown_dat, Err(Error::Refused(_))));
}

#[test]
fn the_default_revision_is_the_latest_audits_else_the_newest_imported() {
    let env = Env::new(20);
    let (first, _) = env.import(1);
    env.add_set();
    assert_eq!(env.audit(None).report.dat_revision.sha256, first);

    let (second, _) = env.import(2);
    assert_eq!(
        env.audit(None).report.dat_revision.sha256,
        first,
        "a newer DAT Revision is adopted explicitly"
    );
    assert_eq!(
        env.audit(Some(&second[..8])).report.dat_revision.sha256,
        second
    );
    assert_eq!(env.audit(None).report.dat_revision.sha256, second);
    let last = env.store.source_sets().unwrap()[0]
        .last_verified_against
        .clone()
        .unwrap();
    assert_eq!(last.sha256, second);

    let fresh = Env::new(20);
    fresh.import(1);
    let (newest, _) = fresh.import(2);
    fresh.add_set();
    assert_eq!(fresh.audit(None).report.dat_revision.sha256, newest);
}

#[test]
fn a_stored_audits_report_equals_the_stateless_audits_apart_from_times() {
    let env = Env::new(20);
    env.import(2);
    env.add_set();
    let set = env.set_dir();
    write_file(&set.join("Pong (USA).a26"), b"pong");
    write_file(&set.join("Combat (Anthology).a26"), b"combat");
    write_file(&set.join("Breakout (USA).a26"), b"pong");
    write_file(&set.join("notes.txt"), b"unmatched");
    write_zip(
        &set.join("Breakout.zip"),
        &[("Breakout (USA).a26", b"break")],
    );
    write_file(&set.join("Broken.zip"), b"not a zip");

    let stored = env.audit(None).report;
    let stateless = title_select::audit(&env.dat(2), &set).unwrap();

    let without_times = |report: &title_select::Report| {
        let mut json = serde_json::to_value(report).unwrap();
        let obj = json.as_object_mut().unwrap();
        obj.remove("started_at").unwrap();
        obj.remove("finished_at").unwrap();
        json
    };
    assert_eq!(without_times(&stored), without_times(&stateless));
    assert_eq!(stored.summary.findings.values().sum::<usize>(), 6);
}

fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime, bool)> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .map(|e| e.unwrap())
        .map(|e| {
            let meta = e.metadata().unwrap();
            let bytes = if meta.is_file() {
                std::fs::read(e.path()).unwrap()
            } else {
                Vec::new()
            };
            (
                e.path().to_path_buf(),
                (
                    bytes,
                    meta.modified().unwrap(),
                    meta.permissions().readonly(),
                ),
            )
        })
        .collect()
}

#[test]
fn a_stored_audit_leaves_the_source_set_unchanged() {
    let env = Env::new(20);
    env.import(1);
    env.add_set();
    write_file(&env.set_dir().join("Pong (USA).a26"), b"pong");
    write_zip(
        &env.set_dir().join("Combat.zip"),
        &[("Combat (USA).a26", b"combat")],
    );
    let before = snapshot(&env.root());

    env.audit(None);

    assert_eq!(snapshot(&env.root()), before);
}

#[test]
fn pruning_keeps_the_latest_audits_and_leaves_summaries() {
    let env = Env::new(2);
    env.import(1);
    env.add_set();
    write_file(&env.set_dir().join("Pong (USA).a26"), b"pong");
    let ids: Vec<i64> = (0..3).map(|_| env.audit(None).id).collect();

    let history = env.store.history("Test - System").unwrap();

    assert_eq!(
        history.iter().map(|a| a.id).collect::<Vec<_>>(),
        vec![ids[2], ids[1], ids[0]]
    );
    assert_eq!(
        history.iter().map(|a| a.pruned).collect::<Vec<_>>(),
        vec![false, false, true]
    );
    let summary = &history[2];
    assert_eq!(summary.findings[&title_select::Finding::Matched], 1);
    assert_eq!(summary.games[&title_select::GameStatus::Complete], 1);
    assert!(matches!(env.store.report(ids[0]), Err(Error::Refused(_))));
    assert_eq!(env.store.report(ids[2]).unwrap().entries.len(), 1);
}

#[test]
fn pruning_never_removes_an_audit_cited_as_provenance() {
    let env = Env::new(1);
    env.import(1);
    env.add_set();
    let cited = env.audit(None).id;
    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    db.execute(
        "INSERT INTO audit_citations (audit_id, cited_by) VALUES (?1, 'test')",
        [cited],
    )
    .unwrap();

    env.audit(None);
    env.audit(None);

    let pruned: Vec<bool> = env
        .store
        .history("Test - System")
        .unwrap()
        .iter()
        .map(|a| a.pruned)
        .collect();
    assert_eq!(pruned, vec![false, true, false]);
    assert!(env.store.report(cited).is_ok());
}

#[test]
fn a_dat_blob_is_deleted_once_no_retained_audit_or_newer_import_holds_it() {
    let env = Env::new(1);
    let (first, _) = env.import(1);
    env.add_set();
    env.audit(None);
    let (second, _) = env.import(2);
    assert!(env.blob_present(&first), "the retained Audit holds it");

    env.audit(Some(&second));

    assert!(!env.blob_present(&first), "its only Audit was pruned");
    assert!(env.blob_present(&second));
    let history = env.store.history("Test - System").unwrap();
    assert_eq!(
        history[1].dat_revision_sha256, first,
        "history still names it"
    );
    let refused = env
        .store
        .audit_source_set(&env.config, "Test - System", Some(&first));
    assert!(matches!(refused, Err(Error::Refused(_))));
}

#[test]
fn the_store_uses_wal_mode() {
    let env = Env::new(20);

    let db = rusqlite::Connection::open(env.store.dir().join("title-select.sqlite")).unwrap();
    let mode: String = db
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();

    assert_eq!(mode, "wal");
}

#[test]
fn a_writable_root_is_refused_in_this_slice() {
    let refused = Config::parse("[roots.sets]\npath = \"/sets\"\nwritable = true\n");

    assert!(refused.is_err());
}
