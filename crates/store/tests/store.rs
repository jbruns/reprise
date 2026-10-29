#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use common::*;
use title_select_store::config::Config;
use title_select_store::{Error, ImportOutcome, Store};

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

    /// Closes the store and opens it again, as a restarted process would. The temp dir is returned so that it
    /// outlives the store.
    fn reopen(self) -> (tempfile::TempDir, Store) {
        let Env { tmp, store, .. } = self;
        let dir = store.dir().to_path_buf();
        drop(store);
        (tmp, Store::open(&dir).unwrap())
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

    fn add_dat_and_set(&self) {
        self.import(1);
        self.add_set();
    }

    fn audit(&self, revision: Option<&str>) -> title_select_store::StoredAudit {
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
    let (_tmp, store) = env.reopen();
    assert!(
        !dats.join(&orphan).exists(),
        "opening the store collects too"
    );
    assert!(store.blob_path(&held).is_file());
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
    let stateless = title_select_engine::audit(&env.dat(2), &set).unwrap();

    let without_times = |report: &title_select_engine::Report| {
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
    assert_eq!(summary.findings[&title_select_engine::Finding::Matched], 1);
    assert_eq!(summary.games[&title_select_engine::GameStatus::Complete], 1);
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

fn database(store: &Store) -> rusqlite::Connection {
    rusqlite::Connection::open(store.dir().join("title-select.sqlite")).unwrap()
}

#[test]
fn a_running_audit_holds_its_dat_revision_until_a_restart_discards_it() {
    let env = Env::new(20);
    let (first, _) = env.import(1);
    env.add_set();
    let db = database(&env.store);
    db.execute(
        "INSERT INTO audits (source_set_id, dat_revision_id, status, source_set_path, started_at)
         SELECT s.id, r.id, 'running', 'set', 'now' FROM source_sets s, dat_revisions r WHERE r.sha256 = ?1",
        [&first],
    )
    .unwrap();

    env.import(2);

    assert!(env.blob_present(&first), "the running Audit holds it");
    assert!(env.store.history("Test - System").unwrap().is_empty());
    let config = env.config.clone();
    let (_tmp, store) = env.reopen();
    let running: i64 = database(&store)
        .query_row("SELECT count(*) FROM audits", [], |r| r.get(0))
        .unwrap();
    assert_eq!(running, 0, "a restart discards the unfinished Audit");
    assert!(
        store.blob_path(&first).exists(),
        "opening the store does not run retention"
    );
    store
        .audit_source_set(&config, "Test - System", None)
        .unwrap();
    assert!(
        !store.blob_path(&first).exists(),
        "the next Audit's retention finds nothing holding it"
    );
}

#[test]
fn a_restored_dat_revision_is_kept_across_commands_until_an_audit_uses_it() {
    let env = Env::new(1);
    let (old, _) = env.import(1);
    env.add_set();
    let (new, _) = env.import(2);
    assert!(!env.blob_present(&old));
    env.import(1);
    let config = env.config.clone();
    let (_tmp, store) = env.reopen();
    let present = |sha: &str| store.blob_path(sha).is_file();
    assert!(present(&old), "opening the store keeps the restored blob");

    store
        .audit_source_set(&config, "Test - System", Some(&new))
        .unwrap();
    assert!(
        present(&old),
        "an Audit of another revision does not drop the restored blob"
    );

    let audit = store
        .audit_source_set(&config, "Test - System", Some(&old))
        .unwrap();
    assert_eq!(audit.report.dat_revision.sha256, old);

    store
        .audit_source_set(&config, "Test - System", Some(&new))
        .unwrap();
    assert!(
        !present(&old),
        "once its Audit is pruned, nothing holds the restored revision"
    );
}

#[test]
fn an_audit_that_fails_leaves_no_row() {
    let env = Env::new(20);
    let (sha, _) = env.import(1);
    env.add_set();
    std::fs::write(env.store.blob_path(&sha), b"<datafile></datafile>").unwrap();

    let failed = env
        .store
        .audit_source_set(&env.config, "Test - System", None);

    assert!(matches!(failed, Err(Error::Io(_))), "{failed:?}");
    let rows: i64 = database(&env.store)
        .query_row("SELECT count(*) FROM audits", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn a_stored_audit_does_not_follow_symlinks_out_of_the_root() {
    let env = Env::new(20);
    env.import(1);
    env.add_set();
    let outside = env.tmp.path().join("outside");
    write_file(&outside.join("Pong (USA).a26"), b"pong");
    std::os::unix::fs::symlink(
        outside.join("Pong (USA).a26"),
        env.set_dir().join("Pong (USA).a26"),
    )
    .unwrap();
    std::os::unix::fs::symlink(&outside, env.set_dir().join("elsewhere")).unwrap();
    write_file(&env.root().join("shared/Combat (USA).a26"), b"combat");
    std::os::unix::fs::symlink(env.root().join("shared"), env.set_dir().join("shared")).unwrap();

    let stored = env.audit(None).report;

    let finding = |path: &str| {
        let entry = stored.entries.iter().find(|e| e.path == path).unwrap();
        (entry.finding, entry.reason.clone())
    };
    let escaped = (
        title_select_engine::Finding::Unreadable,
        Some("resolves outside the Root".to_owned()),
    );
    assert_eq!(finding("Pong (USA).a26"), escaped);
    assert_eq!(finding("elsewhere"), escaped);
    assert_eq!(
        finding("shared/Combat (USA).a26").0,
        title_select_engine::Finding::Matched,
        "a symlink inside the Root is followed"
    );
    assert_eq!(stored.entries.len(), 3, "nothing under `elsewhere` is read");

    let stateless = title_select_engine::audit(&env.dat(1), &env.set_dir()).unwrap();
    assert_eq!(
        stateless.summary.findings[&title_select_engine::Finding::Matched],
        3
    );
}

#[test]
fn an_ambiguous_revision_prefix_is_refused() {
    let env = Env::new(20);
    env.add_dat_and_set();
    let mut seen: BTreeMap<char, String> = BTreeMap::new();
    let mut shared = None;
    for n in 0..17 {
        let path = write_dat_version(
            &env.tmp.path().join("in"),
            &format!("v{n}.dat"),
            &format!("v{n}"),
            &[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])],
        );
        let (rev, _) = env.store.import_dat(&path, "No-Intro", None).unwrap();
        let first = rev.sha256.chars().next().unwrap();
        if seen.insert(first, rev.sha256).is_some() {
            shared = Some(first);
            break;
        }
    }
    let prefix = shared.expect("17 SHA-256s share a first digit").to_string();

    let refused = env
        .store
        .audit_source_set(&env.config, "Test - System", Some(&prefix));

    let Err(Error::Refused(message)) = refused else {
        panic!("expected a refusal: {refused:?}");
    };
    assert!(message.contains("more than one"), "{message}");
}

const WORKERS: title_select_store::Workers = title_select_store::Workers {
    readers: 2,
    hashers: 2,
};

#[test]
fn an_audit_job_moves_from_queued_through_running_to_succeeded() {
    use title_select_store::JobState;
    let env = Env::new(20);
    env.add_dat_and_set();
    let (sha256, _) = env.import(2);

    let queued = env
        .store
        .enqueue_audit("Test - System", Some(&sha256[..12]), "Operator", WORKERS)
        .unwrap();
    assert_eq!(queued.state, JobState::Queued);
    assert_eq!(queued.dat_revision, sha256);
    assert_eq!((queued.actor.as_str(), queued.attempt), ("Operator", 1));

    let running = env.store.claim_job().unwrap().unwrap();
    assert_eq!((running.id, running.state), (queued.id, JobState::Running));
    assert!(env.store.claim_job().unwrap().is_none());

    let files = std::sync::Mutex::new(0);
    let progress = |_: title_select_engine::Progress| *files.lock().unwrap() += 1;
    let done = env
        .store
        .run_audit_job(&env.config, running.id, Some(&progress))
        .unwrap();

    assert_eq!(done.state, JobState::Succeeded);
    let audit = env.store.audit(done.audit.unwrap()).unwrap();
    assert_eq!(audit.dat_revision_sha256, sha256);
    assert_eq!(audit.source_set, "Test - System");
    assert_eq!(env.store.jobs(10).unwrap(), vec![done]);
}

#[test]
fn a_second_audit_of_the_same_source_set_is_refused_naming_the_active_job() {
    let env = Env::new(20);
    env.add_dat_and_set();
    let first = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS)
        .unwrap();

    let second = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS);
    assert!(
        matches!(second, Err(Error::JobActive { job, .. }) if job == first.id),
        "{second:?}"
    );

    env.store.claim_job().unwrap();
    let third = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS);
    assert!(matches!(third, Err(Error::JobActive { job, .. }) if job == first.id));

    env.store
        .run_audit_job(&env.config, first.id, None)
        .unwrap();
    env.store
        .enqueue_audit("Test - System", None, "Operator", WORKERS)
        .unwrap();
}

#[test]
fn a_restart_interrupts_a_running_job_and_queues_it_once_more_then_fails_it() {
    use title_select_store::JobState;
    let env = Env::new(20);
    env.add_dat_and_set();
    let first = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap();
    // A Running Job's Audit row, as a stop part way through would leave it.
    database(&env.store)
        .execute(
            "INSERT INTO audits (source_set_id, dat_revision_id, status, source_set_path, started_at)
             SELECT source_set_id, dat_revision_id, 'running', 'set', 'now' FROM jobs",
            [],
        )
        .unwrap();

    let (tmp, store) = env.reopen();
    let gentle = title_select_store::Workers {
        readers: 1,
        hashers: 1,
    };
    let recovered = store.recover_jobs(gentle).unwrap();

    assert_eq!(recovered.len(), 1);
    let interrupted = &recovered[0].job;
    assert_eq!(
        (interrupted.id, interrupted.state),
        (first.id, JobState::Interrupted)
    );
    assert!(interrupted.error.is_some());
    let again = recovered[0].requeued.clone().unwrap();
    assert_eq!(again.state, JobState::Queued);
    assert_eq!((again.attempt, again.requeued_from), (2, Some(first.id)));
    assert_eq!(again.workers, gentle);
    assert_eq!(again.dat_revision, first.dat_revision);
    assert!(store.history("Test - System").unwrap().is_empty());

    assert_eq!(store.claim_job().unwrap().unwrap().id, again.id);
    let dir = store.dir().to_path_buf();
    drop(store);
    let store = Store::open(&dir).unwrap();
    let recovered = store.recover_jobs(gentle).unwrap();

    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].job.state, JobState::Failed);
    assert!(recovered[0].requeued.is_none());
    assert!(store.claim_job().unwrap().is_none());
    assert!(store.recover_jobs(gentle).unwrap().is_empty());
    drop(tmp);
}

#[test]
fn an_audit_job_that_cannot_read_its_source_set_fails() {
    use title_select_store::JobState;
    let env = Env::new(20);
    env.add_dat_and_set();
    let job = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap();
    std::fs::remove_dir_all(env.set_dir()).unwrap();

    let done = env.store.run_audit_job(&env.config, job.id, None).unwrap();

    assert_eq!(done.state, JobState::Failed);
    assert!(done.error.is_some() && done.audit.is_none());
    assert!(env.store.history("Test - System").unwrap().is_empty());
}

#[test]
fn a_queued_job_holds_its_dat_revision_until_it_runs() {
    use title_select_store::JobState;
    let env = Env::new(20);
    let (first, _) = env.import(1);
    env.add_set();
    let job = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS)
        .unwrap();
    assert_eq!(job.dat_revision, first);

    env.import(2);

    assert!(env.blob_present(&first), "the queued Job holds it");
    env.store.claim_job().unwrap();
    let done = env.store.run_audit_job(&env.config, job.id, None).unwrap();
    assert_eq!(done.state, JobState::Succeeded);
}

#[test]
fn failing_a_job_whose_audit_panicked_discards_its_running_audit_and_releases_its_dat_revision() {
    use title_select_store::JobState;
    let env = Env::new(20);
    let (first, _) = env.import(1);
    env.add_set();
    std::fs::write(env.set_dir().join("Pong (USA).a26"), b"pong").unwrap();
    let job = env
        .store
        .enqueue_audit("Test - System", None, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap();

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let progress = |_: title_select_engine::Progress| panic!("injected");
        env.store
            .run_audit_job(&env.config, job.id, Some(&progress))
    }));
    assert!(panicked.is_err());
    let running = || -> i64 {
        database(&env.store)
            .query_row(
                "SELECT count(*) FROM audits WHERE status = 'running'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(running(), 1, "the panic left the Audit running");

    let failed = env
        .store
        .fail_job(job.id, "the Audit stopped unexpectedly")
        .unwrap();

    assert_eq!(failed.state, JobState::Failed);
    assert_eq!(running(), 0);
    env.import(2);
    assert!(
        !env.blob_present(&first),
        "nothing holds the revision any more"
    );
}
