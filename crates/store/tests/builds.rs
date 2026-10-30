//! Managed Folders, Change Plans and Builds, from synthetic DATs, Source Sets and hand-made Selections.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;
use title_select_api::build::{
    AddManagedFolder, BlockerKind, BuildState, Cause, ItemKind, ItemState, ManagedFolder, Plan,
};
use title_select_api::selection::{
    Alternative, CoverageDrift, Exclusion, Rule, Selection, SelectionGroup, SelectionPins,
    SelectionProfile, SelectionSummary, Winner,
};
use title_select_api::{JobKind, JobState};
use title_select_engine::build::verify;
use title_select_engine::torrentzip::{self, Writer};
use title_select_store::config::Config;
use title_select_store::{Error, Store, Workers};

const SET: &str = "Test - System";
const PROFILE: &str = "Mine";
const WORKERS: Workers = Workers {
    readers: 1,
    hashers: 1,
};

/// The Games of the test DAT: each with one or two ROMs.
fn games() -> Vec<(&'static str, Vec<RomSpec>)> {
    vec![
        ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
        ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        (
            "Breakout (USA)",
            vec![
                rom("Breakout (USA).a26", b"break"),
                rom("Breakout (USA).txt", b"manual"),
            ],
        ),
        ("Ōkami (Japan)", vec![rom("Ōkami (Japan).a26", b"cafe")]),
        ("Yars (USA)", vec![rom("Yars (USA).a26", b"yars")]),
        ("Missing (USA)", vec![rom("Missing (USA).a26", b"missing")]),
        ("Bad: Name (USA)", vec![rom("Bad (USA).a26", b"bad")]),
    ]
}

/// A store, a read-only Root `sets` holding the Source Set, and a writable Root `play` for Managed Folders.
struct Env {
    tmp: tempfile::TempDir,
    store: Store,
    config: Config,
    sha256: String,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let sets = tmp.path().join("roots/sets");
        let play = tmp.path().join("roots/play");
        std::fs::create_dir_all(sets.join(SET)).unwrap();
        std::fs::create_dir_all(&play).unwrap();
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        let config = config(&sets, &play, true);
        let store = Store::open(&tmp.path().join("store")).unwrap();
        let dat = write_dat_version(
            &tmp.path().join("in"),
            "Test - System (1).dat",
            "20260101-000000",
            &games(),
        );
        let (revision, _) = store.import_dat(&dat, "No-Intro", None).unwrap();
        store
            .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
            .unwrap();
        store
            .save_profile(
                PROFILE,
                &SelectionProfile::default(),
                None,
                "Operator",
                &[],
                WORKERS,
            )
            .unwrap();
        let env = Env {
            tmp,
            store,
            config,
            sha256: revision.sha256,
        };
        env.write_sources();
        env
    }

    fn set_dir(&self) -> PathBuf {
        self.tmp.path().join("roots/sets").join(SET)
    }

    fn play(&self) -> PathBuf {
        self.tmp.path().join("roots/play")
    }

    /// Pong: an exact TorrentZip (copied) and a loose Matched file; Combat: an exact ZIP that is not TorrentZip
    /// (repacked); Breakout: a Misnamed file and a Matched one; Ōkami: a ZIP and a Misnamed file before it in path
    /// order; Yars: only a Conflicting file, named as Combat's ROM. Missing has no source.
    fn write_sources(&self) {
        let dir = self.set_dir();
        torrentzip_file(&dir.join("Pong (USA).zip"), &[("Pong (USA).a26", b"pong")]);
        write_file(&dir.join("Pong (USA).a26"), b"pong");
        write_stored_zip(
            &dir.join("Combat (USA).zip"),
            &[("Combat (USA).a26", b"combat")],
        );
        write_file(&dir.join("breakout.bin"), b"break");
        write_file(&dir.join("Breakout (USA).txt"), b"manual");
        write_zip(
            &dir.join("Ōkami (Japan).zip"),
            &[("Ōkami (Japan).a26", b"cafe")],
        );
        write_file(&dir.join("a-cafe.bin"), b"cafe");
        write_file(&dir.join("Combat (USA).a26"), b"yars");
    }

    fn audit(&self) {
        self.store
            .audit_source_set(&self.config, SET, None)
            .unwrap();
    }

    /// Records `selection` as a Selection Job's result, made with the latest Profile version.
    fn record(&self, selection: &Selection) -> i64 {
        self.store
            .enqueue_selection(SET, None, PROFILE, None, "Operator", WORKERS)
            .unwrap();
        let job = self.store.claim_job().unwrap().unwrap();
        assert_eq!(job.kind, JobKind::Selection);
        self.store
            .record_selection(&self.config, job.id, selection)
            .unwrap()
            .selection
            .unwrap()
    }

    fn folder(&self, path: &str) -> ManagedFolder {
        self.store
            .add_managed_folder(&self.config, &add(path))
            .unwrap()
    }

    fn plan(&self, folder: &ManagedFolder, selection: i64) -> Plan {
        self.store
            .plan_build(&self.config, folder.id, selection)
            .unwrap()
    }

    /// Approves `plan`, then claims and runs its Build Job.
    fn build(&self, plan: &Plan) -> title_select_api::Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        assert_eq!(job.kind, JobKind::Build);
        let claimed = self.store.claim_job().unwrap().unwrap();
        assert_eq!(claimed.id, job.id);
        self.store
            .run_build_job(&self.config, job.id, None)
            .unwrap()
    }
}

fn config(sets: &Path, play: &Path, writable: bool) -> Config {
    Config::parse(&format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = {writable}\n",
        sets.display().to_string(),
        play.display().to_string(),
    ))
    .unwrap()
}

fn add(path: &str) -> AddManagedFolder {
    AddManagedFolder {
        root: "play".into(),
        path: Some(path.into()),
        template: None,
        source_set: SET.into(),
        profile: PROFILE.into(),
    }
}

fn torrentzip_file(path: &Path, members: &[(&str, &[u8])]) {
    let mut writer = Writer::new(std::fs::File::create(path).unwrap()).unwrap();
    for (name, payload) in members {
        writer
            .add(name, payload.len() as u64, &mut &payload[..])
            .unwrap();
    }
    writer.finish().unwrap();
}

fn validate(path: &Path) -> Vec<torrentzip::Member> {
    torrentzip::validate(&mut std::fs::File::open(path).unwrap()).unwrap()
}

fn rule(stage: &str) -> Rule {
    Rule {
        stage: stage.into(),
        reference: Some("REF0001".into()),
        message: "m".into(),
    }
}

fn group(title: &str) -> SelectionGroup {
    SelectionGroup {
        group: title.to_lowercase(),
        unresolved: false,
        winners: vec![Winner {
            title: title.into(),
            rule: rule("Parent selection"),
            unresolved: false,
        }],
        alternatives: vec![],
        exclusions: vec![],
        choice: None,
        drift: false,
    }
}

fn selection(sha256: &str, groups: Vec<SelectionGroup>) -> Selection {
    Selection {
        pins: SelectionPins::default(),
        dat_sha256: sha256.into(),
        system: SET.into(),
        system_settings: false,
        summary: SelectionSummary::default(),
        coverage_drift: CoverageDrift::default(),
        groups,
        stale_choices: BTreeMap::new(),
    }
}

/// Every Game of the DAT a winner.
fn everything(sha256: &str) -> Selection {
    selection(sha256, games().iter().map(|(g, _)| group(g)).collect())
}

fn kinds(plan: &Plan) -> BTreeMap<String, ItemKind> {
    plan.content
        .items
        .iter()
        .map(|i| (i.game.clone(), i.kind))
        .collect()
}

fn blockers(plan: &Plan) -> BTreeSet<BlockerKind> {
    plan.content.blockers.iter().map(|b| b.kind).collect()
}

fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn marker(dir: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(dir.join(".title-select")).unwrap()).unwrap()
}

#[test]
fn a_selection_is_built_from_matched_misnamed_and_conflicting_entries_as_torrentzips_under_dat_names()
 {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(names(&dir), BTreeSet::from([".title-select".to_owned()]));
    assert_eq!(marker(&dir)["generation"], 0);
    std::fs::write(dir.join("notes.txt"), b"mine").unwrap();

    let plan = env.plan(&folder, id);

    assert_eq!(
        kinds(&plan),
        BTreeMap::from([
            ("Pong (USA)".to_owned(), ItemKind::Copy),
            ("Combat (USA)".to_owned(), ItemKind::Repack),
            ("Breakout (USA)".to_owned(), ItemKind::Repack),
            ("Ōkami (Japan)".to_owned(), ItemKind::Repack),
            ("Yars (USA)".to_owned(), ItemKind::Repack),
            ("Missing (USA)".to_owned(), ItemKind::NoSource),
            ("Bad: Name (USA)".to_owned(), ItemKind::Blocked),
        ])
    );
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let source = |game: &str| {
        let item = plan.content.items.iter().find(|i| i.game == game).unwrap();
        item.sources
            .iter()
            .map(|s| (s.path.clone(), s.finding))
            .collect::<Vec<_>>()
    };
    use title_select_api::Finding::*;
    // A copy-able ZIP over a loose Matched file before it in path order; Matched over an earlier Misnamed file.
    assert_eq!(source("Pong (USA)"), [("Pong (USA).zip".into(), Matched)]);
    assert_eq!(
        source("Ōkami (Japan)"),
        [("Ōkami (Japan).zip".into(), Matched)]
    );
    assert_eq!(
        source("Breakout (USA)"),
        [
            ("breakout.bin".into(), Misnamed),
            ("Breakout (USA).txt".into(), Matched)
        ]
    );
    assert_eq!(
        source("Yars (USA)"),
        [("Combat (USA).a26".into(), Conflicting)]
    );

    let before = SystemTime::now() - Duration::from_secs(2);
    let job = env.build(&plan);
    let after = SystemTime::now() + Duration::from_secs(2);

    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error);
    assert_eq!(
        names(&dir),
        BTreeSet::from(
            [
                ".title-select",
                "notes.txt",
                "Pong (USA).zip",
                "Combat (USA).zip",
                "Breakout (USA).zip",
                "Ōkami (Japan).zip",
                "Yars (USA).zip",
            ]
            .map(String::from)
        ),
        "no staging is left, nothing is written for the no-source or blocked items"
    );
    assert_eq!(std::fs::read(dir.join("notes.txt")).unwrap(), b"mine");
    assert_eq!(
        std::fs::read(dir.join("Pong (USA).zip")).unwrap(),
        std::fs::read(env.set_dir().join("Pong (USA).zip")).unwrap(),
        "an exact TorrentZip is copied as it is"
    );
    for item in plan.content.items.iter().filter(|i| i.kind.writes()) {
        let path = dir.join(&item.target);
        validate(&path);
        verify(&path, &item.roms).unwrap();
        let mtime = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert!(before <= mtime && mtime <= after, "{}", item.target);
    }
    let okami = validate(&dir.join("Ōkami (Japan).zip"));
    assert!(okami[0].utf8, "a non-ASCII name carries the UTF-8 flag");
    let combat = validate(&dir.join("Combat (USA).zip"));
    assert!(!combat[0].utf8);
    assert_eq!(marker(&dir)["generation"], 1);

    let folder = env.store.managed_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.generation, 1);
    assert_eq!(folder.files, 5);
    let build = folder.build.as_ref().unwrap();
    assert_eq!(build.state, BuildState::Succeeded);
    assert_eq!((build.applied, build.failed, build.pending), (5, 0, 0));
    assert_eq!(build.planned.no_source, 1);
    assert_eq!(build.planned.blocked, 1);
    let files = env.store.built_files(folder.id).unwrap();
    let pong = files.iter().find(|f| f.game == "Pong (USA)").unwrap();
    assert_eq!(pong.kind, ItemKind::Copy);
    assert_eq!((pong.selection, Some(pong.audit)), (id, plan.content.audit));
    assert_eq!(pong.dat_revision, env.sha256);
    assert_eq!(pong.sources[0].path, "Pong (USA).zip");
    assert_eq!(
        pong.file.size,
        std::fs::metadata(dir.join("Pong (USA).zip")).unwrap().len()
    );

    // Planned again with nothing changed, every built file is unchanged.
    let again = env.plan(&folder, id);
    for (before, now) in plan.content.items.iter().zip(&again.content.items) {
        let expected = if before.kind.writes() {
            ItemKind::Unchanged
        } else {
            before.kind
        };
        assert_eq!(now.kind, expected, "{}", now.target);
    }
}

#[test]
fn the_digest_binds_the_plan_and_a_stale_approval_is_rejected_with_the_new_plan() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");

    let plan = env.plan(&folder, id);
    let same = env.plan(&folder, id);
    assert_eq!(
        plan.digest, same.digest,
        "the same inputs make the same plan"
    );
    assert_eq!(plan.digest.len(), 64);
    let bytes = serde_json::to_vec(&plan.content).unwrap();
    let hex: String = <sha2::Sha256 as sha2::Digest>::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(plan.digest, hex);

    let wrong = env
        .store
        .approve_plan(&env.config, plan.id, &"0".repeat(64), "Operator", WORKERS);
    assert!(matches!(wrong, Err(Error::Conflict(_))), "{wrong:?}");

    // A source's mtime is part of the plan.
    let source = env.set_dir().join("Combat (USA).zip");
    let file = std::fs::File::options().write(true).open(&source).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(file);
    let stale = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS);
    let Err(Error::Conflict(message)) = stale else {
        panic!("{stale:?}")
    };
    let newer = env.store.plan(plan.id + 2).unwrap();
    assert!(
        message.contains(&format!("review plan {}", newer.id)),
        "{message}"
    );
    assert_ne!(newer.digest, plan.digest);
    assert!(env.store.plan(plan.id).unwrap().approval.is_none());

    // Approved, then changed before the Build runs: the Job fails and nothing is written.
    let job = env
        .store
        .approve_plan(&env.config, newer.id, &newer.digest, "Operator", WORKERS)
        .unwrap();
    std::fs::write(env.play().join("atari2600/Pong (USA).zip"), b"theirs").unwrap();
    env.store.claim_job().unwrap();
    let failed = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(failed.state, JobState::Failed);
    assert!(
        failed
            .error
            .as_deref()
            .unwrap()
            .contains("the plan changed since you approved it"),
        "{:?}",
        failed.error
    );
    assert_eq!(
        names(&env.play().join("atari2600")),
        BTreeSet::from([".title-select".to_owned(), "Pong (USA).zip".to_owned()])
    );
}

#[test]
fn an_unmanaged_file_at_a_target_is_a_conflict_left_untouched() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    std::fs::write(dir.join("combat (usa).zip"), b"theirs").unwrap();

    let plan = env.plan(&folder, id);

    let combat = plan
        .content
        .items
        .iter()
        .find(|i| i.game == "Combat (USA)")
        .unwrap();
    assert_eq!(combat.kind, ItemKind::Conflict);
    assert!(plan.approvable());
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        std::fs::read(dir.join("combat (usa).zip")).unwrap(),
        b"theirs"
    );
    assert!(!names(&dir).contains("Combat (USA).zip"));
}

#[test]
fn a_dat_revision_mismatch_and_an_unresolved_group_block_the_plan() {
    let env = Env::new();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");

    let unaudited = env.plan(&folder, id);
    assert_eq!(blockers(&unaudited), BTreeSet::from([BlockerKind::NoAudit]));

    // The latest Audit is against a newer revision.
    let mut newer = games();
    newer.push(("Extra (USA)", vec![rom("Extra (USA).a26", b"extra")]));
    let dat = write_dat_version(
        &env.tmp.path().join("in"),
        "Test - System (2).dat",
        "20260102-000000",
        &newer,
    );
    env.store.import_dat(&dat, "No-Intro", None).unwrap();
    env.audit();
    let mismatched = env.plan(&folder, id);
    assert_eq!(
        blockers(&mismatched),
        BTreeSet::from([BlockerKind::RevisionMismatch])
    );
    assert!(mismatched.content.items.is_empty());
    let refused = env.store.approve_plan(
        &env.config,
        mismatched.id,
        &mismatched.digest,
        "Operator",
        WORKERS,
    );
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");

    let env = Env::new();
    env.audit();
    let mut unresolved = everything(&env.sha256);
    unresolved.groups[1].unresolved = true;
    let id = env.record(&unresolved);
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Unresolved]));
    assert_eq!(
        plan.content.blockers[0].group.as_deref(),
        Some("combat (usa)")
    );
    assert!(!plan.approvable());
}

#[test]
fn a_release_no_longer_selected_needs_a_later_build_step() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    env.build(&env.plan(&folder, id));

    let fewer = selection(&env.sha256, vec![group("Pong (USA)")]);
    let id = env.record(&fewer);
    let plan = env.plan(&folder, id);

    assert!(blockers(&plan).contains(&BlockerKind::Removal));
    assert!(
        plan.content
            .blockers
            .iter()
            .all(|b| b.message.contains("needs a later build step"))
    );
    assert!(!plan.approvable());
}

#[test]
fn registration_needs_a_writable_root_and_an_empty_folder_and_a_plan_checks_the_marker() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));

    let mut read_only = add("x");
    read_only.root = "sets".into();
    let refused = env.store.add_managed_folder(&env.config, &read_only);
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    std::fs::create_dir_all(env.play().join("full")).unwrap();
    std::fs::write(env.play().join("full/game.zip"), b"x").unwrap();
    let full = env.store.add_managed_folder(&env.config, &add("full"));
    assert!(matches!(full, Err(Error::Refused(_))), "{full:?}");

    // Two Managed Folders for one system.
    let first = env.folder("atari2600");
    let second = env.folder("atari2600-b");
    assert_ne!(first.id, second.id);
    let again = env.store.add_managed_folder(&env.config, &add("atari2600"));
    assert!(again.is_err());

    // Another folder's marker, as a wrong mount would show.
    let dir = env.play().join("atari2600");
    std::fs::copy(
        env.play().join("atari2600-b/.title-select"),
        dir.join(".title-select"),
    )
    .unwrap();
    let plan = env.plan(&first, id);
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Marker]));

    // Another store's marker.
    let mut foreign = marker(&env.play().join("atari2600-b"));
    foreign["store"] = "elsewhere".into();
    foreign["folder"] = first.id.into();
    std::fs::write(dir.join(".title-select"), foreign.to_string()).unwrap();
    assert_eq!(
        blockers(&env.plan(&first, id)),
        BTreeSet::from([BlockerKind::Marker])
    );

    // A generation newer than the store's, as a restored store would show.
    let mut ahead = marker(&env.play().join("atari2600-b"));
    ahead["folder"] = first.id.into();
    ahead["generation"] = 3.into();
    std::fs::write(dir.join(".title-select"), ahead.to_string()).unwrap();
    let plan = env.plan(&first, id);
    assert!(plan.content.blockers[0].message.contains("newer"));

    // A missing marker on a folder with files.
    std::fs::remove_file(dir.join(".title-select")).unwrap();
    std::fs::write(dir.join("readme.txt"), b"x").unwrap();
    assert_eq!(
        blockers(&env.plan(&first, id)),
        BTreeSet::from([BlockerKind::Marker])
    );

    // The second folder builds; a Root made read-only blocks its next plan.
    assert_eq!(env.build(&env.plan(&second, id)).state, JobState::Succeeded);
    let read_only = config(&env.tmp.path().join("roots/sets"), &env.play(), false);
    let plan = env.store.plan_build(&read_only, second.id, id).unwrap();
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::ReadOnlyRoot]));
}

#[test]
fn a_source_that_fails_its_rehash_fails_only_its_item() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    // The same size and mtime, so the plan still holds, but another payload.
    let source = env.set_dir().join("Combat (USA).zip");
    let mtime = std::fs::metadata(&source).unwrap().modified().unwrap();
    let bytes = std::fs::read(&source).unwrap();
    let at = bytes.windows(6).position(|w| w == b"combat").unwrap();
    let mut altered = bytes.clone();
    altered[at..at + 6].copy_from_slice(b"COMBAT");
    std::fs::write(&source, &altered).unwrap();
    let file = std::fs::File::options().write(true).open(&source).unwrap();
    file.set_modified(mtime).unwrap();
    drop(file);

    let job = env.build(&plan);

    assert_eq!(job.state, JobState::Succeeded);
    let dir = env.play().join("atari2600");
    assert!(!names(&dir).contains("Combat (USA).zip"));
    assert!(names(&dir).contains("Pong (USA).zip"));
    assert!(!dir.join(".title-select-staging").exists());
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    let combat = detail
        .items
        .iter()
        .find(|i| i.game == "Combat (USA)")
        .unwrap();
    assert_eq!(combat.state, ItemState::Failed);
    assert!(combat.error.is_some());
    assert_eq!((detail.build.applied, detail.build.failed), (4, 1));
}

#[test]
fn an_interrupted_build_leaves_state_that_recovery_cleans_up() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let job = env
        .store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    env.store.claim_job().unwrap();
    let dir = env.play().join("atari2600");

    let stop = |p: title_select_engine::Progress| {
        if p.files_done == 2 {
            panic!("the service stops");
        }
    };
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        env.store.run_build_job(&env.config, job.id, Some(&stop))
    }));
    assert!(ran.is_err());
    assert!(dir.join(".title-select-staging").is_dir());

    let Env {
        tmp, store, config, ..
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let store = Store::open(&path).unwrap();
    let interrupted = store.recover_jobs(WORKERS).unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].job.state, JobState::Interrupted);
    assert!(
        interrupted[0].requeued.is_none(),
        "a build is never queued again"
    );
    store.recover_builds(&config).unwrap();

    assert!(!dir.join(".title-select-staging").exists());
    assert_eq!(marker(&dir)["generation"], 0);
    let build = store.build_of_job(job.id).unwrap().unwrap();
    let detail = store.build(build.id).unwrap();
    assert_eq!(detail.build.state, BuildState::Interrupted);
    let states: Vec<ItemState> = detail.items.iter().map(|i| i.state).collect();
    assert_eq!(
        states,
        [
            ItemState::Applied,
            ItemState::Applied,
            ItemState::Pending,
            ItemState::Pending,
            ItemState::Pending
        ]
    );
    // A new plan starts from the same generation and keeps the applied files.
    let next = store.plan_build(&config, folder.id, id).unwrap();
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(next.content.generation, 0);
    assert_eq!(next.summary.unchanged, 2);
    drop(tmp);
}

#[test]
fn the_preview_attributes_each_change_to_settings_choices_or_the_dat_revision() {
    let env = Env::new();
    env.audit();
    let first = selection(
        &env.sha256,
        vec![
            group("Pong (USA)"),
            group("Combat (USA)"),
            group("Breakout (USA)"),
        ],
    );
    let id = env.record(&first);
    let folder = env.folder("atari2600");
    env.build(&env.plan(&folder, id));

    let profile = SelectionProfile {
        region_order: vec!["USA".into()],
        ..SelectionProfile::default()
    };
    env.store
        .save_profile(PROFILE, &profile, Some(1), "Operator", &[], WORKERS)
        .unwrap();
    let mut combat = group("Combat (USA)");
    let lost = combat.winners.pop().unwrap();
    combat.alternatives.push(Alternative {
        title: lost.title,
        clone_of: "Pong (USA)".into(),
        rule: Rule::choice(),
    });
    let mut breakout = group("Breakout (USA)");
    let dropped = breakout.winners.pop().unwrap();
    breakout.exclusions.push(Exclusion {
        title: dropped.title,
        rule: rule("Region"),
        reason: "Region".into(),
    });
    let second = selection(
        &env.sha256,
        vec![group("Pong (USA)"), combat, breakout, group("Yars (USA)")],
    );
    let id = env.record(&second);

    let plan = env.plan(&folder, id);

    let causes: BTreeMap<&str, Cause> = plan
        .changes
        .iter()
        .map(|c| (c.title.as_str(), c.cause))
        .collect();
    assert_eq!(
        causes,
        BTreeMap::from([
            ("Combat (USA)", Cause::Choices),
            ("Breakout (USA)", Cause::Settings),
            ("Yars (USA)", Cause::DatRevision),
        ])
    );
}

#[test]
fn the_current_build_keeps_its_selection_and_audit_from_pruning_until_a_later_build() {
    let mut env = Env::new();
    env.config = Config::parse(&format!(
        "[roots.sets]\npath = {:?}\n\n[roots.play]\npath = {:?}\nwritable = true\n\n[audits]\nkeep = 1\n\n\
         [selections]\nkeep = 1\n",
        env.tmp.path().join("roots/sets").display().to_string(),
        env.play().display().to_string(),
    ))
    .unwrap();
    env.audit();
    let first = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, first);
    let audit = plan.content.audit.unwrap();
    env.build(&plan);

    env.audit();
    let second = env.record(&everything(&env.sha256));

    assert!(!env.store.selection_record(first).unwrap().pruned);
    assert!(!env.store.audit(audit).unwrap().pruned);
    // A later build releases them.
    env.audit();
    let third = env.record(&everything(&env.sha256));
    let plan = env.plan(&folder, third);
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    env.build(&plan);
    env.audit();
    env.record(&everything(&env.sha256));
    assert!(env.store.selection_record(first).unwrap().pruned);
    assert!(env.store.selection_record(second).unwrap().pruned);
    assert!(env.store.audit(audit).unwrap().pruned);
}
