//! Managed Folders, Change Plans and Builds, from synthetic DATs, Source Sets and hand-made Selections.

#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;
use title_select_api::build::{
    AddManagedFolder, BlockerKind, BuildState, Cause, DriftKind, FileOutcome, FolderCheck,
    IncompleteReason, ItemKind, ItemState, ManagedFolder, Plan, PlanBuild, PlanChoices,
};
use title_select_api::selection::{
    Alternative, CoverageDrift, Exclusion, Rule, Selection, SelectionGroup, SelectionPins,
    SelectionProfile, SelectionSummary, Winner,
};
use title_select_api::{JobKind, JobState};
use title_select_engine::build::verify;
use title_select_engine::torrentzip::{self, Writer};
use title_select_store::config::Config;
use title_select_store::{Error, Fault, Store, Workers};

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
        ("Sub (USA)", vec![rom("sub/Sub (USA).a26", b"sub")]),
        ("Back (USA)", vec![rom("..\\Back (USA).a26", b"back")]),
        ("Up (USA)", vec![rom("..", b"up")]),
        ("Root (USA)", vec![rom("/Root (USA).a26", b"root")]),
        ("Device (USA)", vec![rom("CON.a26", b"con")]),
        ("Colon (USA)", vec![rom("C:Colon (USA).a26", b"colon")]),
        ("LPT1", vec![rom("LPT1.a26", b"lpt")]),
        ("aux", vec![rom("aux.a26", b"aux")]),
    ]
}

/// The Games whose names cannot be written into a Managed Folder: a ROM name with a path separator, a parent
/// component, an absolute path, a device name or a drive, and Game names that are device names.
const UNSAFE: [&str; 8] = [
    "Sub (USA)",
    "Back (USA)",
    "Up (USA)",
    "Root (USA)",
    "Device (USA)",
    "Colon (USA)",
    "LPT1",
    "aux",
];

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

    /// Approves `plan`, claims its Build Job and runs it with `progress`.
    fn build_with(
        &self,
        plan: &Plan,
        progress: &(dyn Fn(title_select_engine::Progress) + Sync),
    ) -> title_select_api::Job {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store
            .run_build_job(&self.config, job.id, Some(progress))
            .unwrap()
    }

    /// Approves `plan` and runs its Build Job until `fault` stops it as a kill would. Returns the Job's id.
    fn crash(&self, plan: &Plan, fault: Fault) -> i64 {
        let job = self
            .store
            .approve_plan(&self.config, plan.id, &plan.digest, "Operator", WORKERS)
            .unwrap();
        self.store.claim_job().unwrap().unwrap();
        self.store.inject_fault(Some(fault));
        let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.store.run_build_job(&self.config, job.id, None)
        }));
        assert!(ran.is_err(), "{ran:?}");
        job.id
    }

    /// Opens the store again, as the service does when it starts, and recovers. Returns the Builds whose folders
    /// recovery left as they were.
    fn restart(self) -> (Env, Vec<(i64, String)>) {
        let Env {
            tmp,
            store,
            config,
            sha256,
        } = self;
        let path = store.dir().to_path_buf();
        drop(store);
        let store = Store::open(&path).unwrap();
        store.recover_jobs(WORKERS).unwrap();
        let blocked = store.recover_builds(&config).unwrap();
        (
            Env {
                tmp,
                store,
                config,
                sha256,
            },
            blocked,
        )
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
    selection(
        sha256,
        games()
            .iter()
            .filter(|(g, _)| !UNSAFE.contains(g))
            .map(|(g, _)| group(g))
            .collect(),
    )
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
        verify(&std::fs::File::open(&path).unwrap(), &item.roms).unwrap();
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

/// The files of `plan` a Build writes, in the order it writes them.
fn written(plan: &Plan) -> Vec<String> {
    plan.content
        .items
        .iter()
        .filter(|i| i.kind.writes())
        .map(|i| i.target.clone())
        .collect()
}

#[test]
fn a_crash_between_putting_a_file_in_place_and_recording_it_is_settled_from_its_publish_intent() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");

    let job = env.crash(&plan, Fault::AfterPublish(1));
    assert!(dir.join(&targets[1]).is_file());
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    let build = env.store.build_of_job(job).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.build.state, BuildState::Interrupted);
    assert_eq!(detail.items[1].state, ItemState::Applied, "{detail:?}");
    assert_eq!(detail.items[2].state, ItemState::Pending);
    let files = env.store.built_files(folder.id).unwrap();
    assert!(files.iter().any(|f| f.target == targets[1]));
    assert!(!dir.join(".title-select-staging").exists());
    assert_eq!(marker(&dir)["generation"], 0);
    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    let item = next
        .content
        .items
        .iter()
        .find(|i| i.target == targets[1])
        .unwrap();
    assert_eq!(item.kind, ItemKind::Unchanged, "adopted, not a conflict");
}

#[test]
fn a_file_changed_at_its_target_before_recovery_is_left_untouched_and_reported_uncertain() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");

    let job = env.crash(&plan, Fault::AfterPublish(1));
    std::fs::write(dir.join(&targets[1]), b"not the build's").unwrap();
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    let build = env.store.build_of_job(job).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Uncertain);
    assert!(
        detail.items[1]
            .error
            .as_ref()
            .unwrap()
            .contains("left as it is")
    );
    assert_eq!(
        std::fs::read(dir.join(&targets[1])).unwrap(),
        b"not the build's"
    );
    let files = env.store.built_files(folder.id).unwrap();
    assert!(!files.iter().any(|f| f.target == targets[1]));
    let next = env.plan(&folder, id);
    let item = next
        .content
        .items
        .iter()
        .find(|i| i.target == targets[1])
        .unwrap();
    assert_eq!(item.kind, ItemKind::Conflict);
}

#[test]
fn a_marker_that_did_not_follow_the_committed_generation_is_moved_forward() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");

    let job = env.crash(&plan, Fault::AfterFinalize);
    assert_eq!(marker(&dir)["generation"], 0, "the store decided first");
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(marker(&dir)["generation"], 1, "and the marker followed");
    assert!(!dir.join(".title-select.new").exists());
    let build = env.store.build_of_job(job).unwrap().unwrap();
    assert_eq!(build.state, BuildState::Interrupted);
    assert_eq!(build.applied, written(&plan).len() as u64);
    let folder = env.store.managed_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.generation, 1);
    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(next.content.generation, 1);
    assert_eq!(next.summary.unchanged, written(&plan).len() as u64);
}

#[test]
fn a_stop_after_the_marker_advanced_needs_nothing_put_back() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");

    env.crash(&plan, Fault::AfterMarker);
    let (env, blocked) = env.restart();

    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(marker(&dir)["generation"], 1);
    let next = env.plan(&folder, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(next.content.generation, 1);
}

#[test]
fn a_backup_taken_mid_finalize_and_restored_never_moves_the_marker_back() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");
    let backup = env.tmp.path().join("backup");
    let (from, to) = (env.store.dir().to_path_buf(), backup.clone());
    env.store.hook_faults(Some(std::sync::Arc::new(move |at| {
        if at == Fault::AfterFinalize {
            copy_dir(&from, &to);
        }
    })));

    assert_eq!(env.build(&plan).state, JobState::Succeeded);
    assert_eq!(marker(&dir)["generation"], 1);

    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    drop(store);
    let store = Store::open(&backup).unwrap();
    store.recover_jobs(WORKERS).unwrap();
    let blocked = store.recover_builds(&config).unwrap();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert_eq!(marker(&dir)["generation"], 1, "never moved back");
    let plan = store.plan_build(&config, folder.id, id).unwrap();
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    assert_eq!(plan.content.generation, 1);
    drop((tmp, sha256));
}

#[test]
fn a_store_restored_one_generation_behind_its_folder_blocks_and_recovery_leaves_the_marker() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    // A backup of the store, taken before the folder's first Build.
    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    let path = store.dir().to_path_buf();
    drop(store);
    let backup = tmp.path().join("backup");
    copy_dir(&path, &backup);
    let env = Env {
        store: Store::open(&path).unwrap(),
        tmp,
        config,
        sha256,
    };
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);
    assert_eq!(marker(&dir)["generation"], 1);

    let Env {
        tmp,
        store,
        config,
        sha256,
    } = env;
    drop(store);
    let store = Store::open(&backup).unwrap();
    store.recover_jobs(WORKERS).unwrap();
    let blocked = store.recover_builds(&config).unwrap();
    assert!(
        blocked.is_empty(),
        "the restored store has no build to recover"
    );
    assert_eq!(marker(&dir)["generation"], 1, "the marker is left as it is");
    let plan = store.plan_build(&config, folder.id, id).unwrap();
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Marker]));
    assert!(plan.content.blockers[0].message.contains("restored"));
    drop((tmp, sha256));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[test]
fn recovery_leaves_a_folder_whose_marker_is_not_the_stores_and_leftovers_no_build_accounts_for() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let other = env.folder("atari2600-b");
    let dir = env.play().join("atari2600");
    let plan = env.plan(&folder, id);
    let job = env.crash(&plan, Fault::AfterPublish(1));
    assert!(dir.join(".title-select-staging").is_dir());
    let mut foreign = marker(&dir);
    foreign["store"] = "elsewhere".into();
    std::fs::write(dir.join(".title-select"), foreign.to_string()).unwrap();
    // A staging directory in a folder no unfinished Build of the store accounts for.
    let other_dir = env.play().join("atari2600-b");
    std::fs::create_dir(other_dir.join(".title-select-staging")).unwrap();
    std::fs::write(other_dir.join(".title-select-staging/0.zip"), b"x").unwrap();

    let (env, blocked) = env.restart();

    let build = env.store.build_of_job(job).unwrap().unwrap();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].0, build.id);
    assert!(blocked[0].1.contains("another store"), "{blocked:?}");
    assert!(dir.join(".title-select-staging").is_dir(), "left as it is");
    assert_eq!(marker(&dir)["store"], "elsewhere");
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Publishing, "not settled");
    let plan = env.plan(&folder, id);
    assert!(blockers(&plan).contains(&BlockerKind::Recovery));
    assert!(other_dir.join(".title-select-staging/0.zip").is_file());
    let plan = env.plan(&other, id);
    assert_eq!(blockers(&plan), BTreeSet::from([BlockerKind::Recovery]));
    assert!(plan.content.blockers[0].message.contains("remove it"));

    // Once the marker is the store's again, the next start cleans up.
    let mut ours = marker(&dir);
    ours["store"] = marker(&other_dir)["store"].clone();
    std::fs::write(dir.join(".title-select"), ours.to_string()).unwrap();
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!dir.join(".title-select-staging").exists());
    assert!(env.plan(&folder, id).approvable());
}

#[cfg(unix)]
#[test]
fn a_symlink_planted_at_the_markers_temp_file_is_never_followed() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    let victim = env.tmp.path().join("victim.txt");
    std::fs::write(&victim, b"precious").unwrap();
    let plan = env.plan(&folder, id);

    let plant = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            std::os::unix::fs::symlink(&victim, dir.join(".title-select.new")).unwrap();
        }
    };
    let job = env.build_with(&plan, &plant);

    assert_eq!(job.state, JobState::Failed);
    assert_eq!(std::fs::read(&victim).unwrap(), b"precious");
    assert_eq!(marker(&dir)["generation"], 0, "the marker did not advance");
    assert!(
        std::fs::symlink_metadata(dir.join(".title-select.new"))
            .unwrap()
            .is_symlink(),
        "recovery leaves what no build wrote"
    );
    let next = env.plan(&folder, id);
    assert!(blockers(&next).contains(&BlockerKind::Recovery));
}

#[test]
fn a_marker_changed_during_a_build_stops_it_before_the_next_file_is_put_in_place() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);

    let swap = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            let mut m = marker(&dir);
            m["generation"] = 5.into();
            std::fs::write(dir.join(".title-select"), m.to_string()).unwrap();
        }
    };
    let job = env.build_with(&plan, &swap);

    assert_eq!(job.state, JobState::Failed);
    assert!(job.error.as_ref().unwrap().contains("marker"), "{job:?}");
    assert!(dir.join(&targets[0]).is_file());
    for target in &targets[1..] {
        assert!(!dir.join(target).exists(), "{target}");
    }
    assert_eq!(marker(&dir)["generation"], 5, "left as it is");
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    assert_eq!(build.state, BuildState::Failed);
    assert_eq!(build.applied, 1);
}

#[cfg(unix)]
#[test]
fn a_folder_swapped_for_a_symlink_mid_build_never_redirects_a_write() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    let moved = env.play().join("moved");
    let elsewhere = env.tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::fs::write(elsewhere.join(".title-select"), b"someone else's").unwrap();
    let plan = env.plan(&folder, id);
    let targets = written(&plan);

    let swap = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            std::fs::rename(&dir, &moved).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
        }
    };
    let job = env.build_with(&plan, &swap);

    assert_eq!(job.state, JobState::Succeeded, "{:?}", job.error);
    assert_eq!(
        names(&elsewhere),
        BTreeSet::from([".title-select".to_owned()])
    );
    assert_eq!(
        std::fs::read(elsewhere.join(".title-select")).unwrap(),
        b"someone else's"
    );
    for target in &targets {
        assert!(moved.join(target).is_file(), "{target}");
    }
    assert_eq!(marker(&moved)["generation"], 1);
    // The path is now a symlink, which a managed folder's path never is.
    let next = env.plan(&folder, id);
    assert_eq!(
        blockers(&next),
        BTreeSet::from([BlockerKind::FolderMissing])
    );
    assert!(
        env.store
            .add_managed_folder(&env.config, &add("atari2600/sub"))
            .is_err()
    );
}

#[test]
fn rom_names_with_path_parts_and_device_names_are_blocked_items() {
    let env = Env::new();
    env.audit();
    let id = env.record(&selection(
        &env.sha256,
        UNSAFE
            .iter()
            .map(|g| group(g))
            .chain([group("Pong (USA)")])
            .collect(),
    ));
    let folder = env.folder("atari2600");

    let plan = env.plan(&folder, id);

    for game in UNSAFE {
        let item = plan.content.items.iter().find(|i| i.game == game).unwrap();
        assert_eq!(item.kind, ItemKind::Blocked, "{game}");
        assert!(item.reason.is_some(), "{game}");
    }
    assert!(plan.approvable(), "{:?}", plan.content.blockers);
    let job = env.build(&plan);
    assert_eq!(job.state, JobState::Succeeded);
    assert_eq!(
        names(&env.play().join("atari2600")),
        BTreeSet::from([".title-select".to_owned(), "Pong (USA).zip".to_owned()])
    );
}

#[test]
fn a_staged_file_replaced_after_it_was_verified_is_never_put_in_place() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");
    let staging = dir.join(".title-select-staging");
    let mode = std::sync::Arc::new(std::sync::Mutex::new(None));
    let (seen, at) = (mode.clone(), staging.clone());
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::BeforePublish(1) {
                use std::os::unix::fs::PermissionsExt;
                *seen.lock().unwrap() =
                    Some(std::fs::metadata(&at).unwrap().permissions().mode() & 0o777);
                std::fs::remove_file(at.join("1.zip")).unwrap();
                std::fs::write(at.join("1.zip"), b"swapped in").unwrap();
            }
        })));

    let job = env.build(&plan);

    assert_eq!(*mode.lock().unwrap(), Some(0o700), "staging is private");
    assert_eq!(job.state, JobState::Failed);
    assert!(
        job.error
            .unwrap()
            .contains("replaced after it was verified")
    );
    assert!(dir.join(&targets[0]).is_file());
    assert!(!dir.join(&targets[1]).exists(), "never renamed");
    assert_eq!(
        std::fs::read(staging.join("1.zip")).unwrap(),
        b"swapped in",
        "left where it is"
    );
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Failed);
    assert_eq!(marker(&dir)["generation"], 0);
    let next = env.plan(&folder, id);
    assert_eq!(blockers(&next), BTreeSet::from([BlockerKind::Recovery]));
    assert!(next.content.blockers[0].message.contains("1.zip"));
}

#[test]
fn a_file_replaced_at_its_target_once_renamed_is_left_and_reported_uncertain() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let targets = written(&plan);
    let dir = env.play().join("atari2600");
    let target = dir.join(&targets[1]);
    let (at, other) = (target.clone(), env.tmp.path().join("other.zip"));
    env.store
        .hook_faults(Some(std::sync::Arc::new(move |fault| {
            if fault == Fault::AfterPublish(1) {
                std::fs::write(&other, b"someone else's").unwrap();
                std::fs::rename(&other, &at).unwrap();
            }
        })));

    let job = env.build(&plan);

    assert_eq!(job.state, JobState::Failed);
    assert_eq!(std::fs::read(&target).unwrap(), b"someone else's");
    let build = env.store.build_of_job(job.id).unwrap().unwrap();
    let detail = env.store.build(build.id).unwrap();
    assert_eq!(detail.items[1].state, ItemState::Uncertain);
    assert!(
        !env.store
            .built_files(folder.id)
            .unwrap()
            .iter()
            .any(|f| f.target == targets[1])
    );
    assert!(
        !dir.join(".title-select-staging").exists(),
        "recovered at once"
    );
    assert!(!blockers(&env.plan(&folder, id)).contains(&BlockerKind::Recovery));
    assert_eq!(marker(&dir)["generation"], 0);
}

#[test]
fn recovery_removes_only_entries_it_recorded_creating() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");
    let staging = dir.join(".title-select-staging");

    // A staging directory swapped for a real one, holding a file named as a staged file.
    env.crash(&plan, Fault::AfterPublish(1));
    std::fs::rename(&staging, env.tmp.path().join("ours")).unwrap();
    std::fs::create_dir(&staging).unwrap();
    std::fs::write(staging.join("2.zip"), b"precious").unwrap();
    // A marker temp file no build wrote.
    std::fs::write(dir.join(".title-select.new"), b"also precious").unwrap();
    let (env, blocked) = env.restart();

    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(blocked[0].1.contains(".title-select.new"), "{blocked:?}");
    assert_eq!(
        std::fs::read(dir.join(".title-select.new")).unwrap(),
        b"also precious"
    );
    assert_eq!(std::fs::read(staging.join("2.zip")).unwrap(), b"precious");

    std::fs::remove_file(dir.join(".title-select.new")).unwrap();
    let (env, blocked) = env.restart();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(
        blocked[0].1.contains(".title-select-staging"),
        "{blocked:?}"
    );
    assert_eq!(std::fs::read(staging.join("2.zip")).unwrap(), b"precious");
    let next = env.plan(&folder, id);
    assert!(blockers(&next).contains(&BlockerKind::Recovery));
    assert!(
        next.content
            .blockers
            .iter()
            .any(|b| b.message.contains("Remove it"))
    );

    // With the build's own staging directory back, a file in it the build did not create is still left.
    std::fs::remove_dir_all(&staging).unwrap();
    std::fs::rename(env.tmp.path().join("ours"), &staging).unwrap();
    std::fs::write(staging.join("extra.zip"), b"mine").unwrap();
    let (env, blocked) = env.restart();
    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(blocked[0].1.contains("extra.zip"), "{blocked:?}");
    assert_eq!(std::fs::read(staging.join("extra.zip")).unwrap(), b"mine");

    std::fs::remove_file(staging.join("extra.zip")).unwrap();
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!staging.exists());
    assert!(env.plan(&folder, id).approvable());
}

#[test]
fn recovery_waits_while_the_root_is_read_only() {
    let env = Env::new();
    env.audit();
    let id = env.record(&everything(&env.sha256));
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    let dir = env.play().join("atari2600");

    env.crash(&plan, Fault::AfterPublish(1));
    let mut env = env;
    env.config = config(&env.tmp.path().join("roots/sets"), &env.play(), false);
    let (env, blocked) = env.restart();

    assert_eq!(blocked.len(), 1, "{blocked:?}");
    assert!(blocked[0].1.contains("not writable"), "{blocked:?}");
    assert!(dir.join(".title-select-staging").is_dir(), "left as it is");
    let next = env.plan(&folder, id);
    assert!(blockers(&next).contains(&BlockerKind::ReadOnlyRoot));
    assert!(blockers(&next).contains(&BlockerKind::Recovery));

    let mut env = env;
    env.config = config(&env.tmp.path().join("roots/sets"), &env.play(), true);
    let (env, blocked) = env.restart();
    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(!dir.join(".title-select-staging").exists());
    assert!(env.plan(&folder, id).approvable());
}

#[test]
fn a_builds_selection_and_audit_are_held_from_pruning_while_it_runs_and_released_when_it_fails() {
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

    let prune = |p: title_select_engine::Progress| {
        if p.files_done == 1 {
            env.audit();
            env.record(&everything(&env.sha256));
            assert!(!env.store.selection_record(first).unwrap().pruned);
            assert!(!env.store.audit(audit).unwrap().pruned);
        }
    };
    assert_eq!(env.build_with(&plan, &prune).state, JobState::Succeeded);
    assert!(!env.store.selection_record(first).unwrap().pruned);
    assert!(!env.store.audit(audit).unwrap().pruned);

    // A Build that fails releases its hold.
    let second = env.record(&everything(&env.sha256));
    let plan = env.plan(&folder, second);
    let held = plan.content.audit.unwrap();
    env.store
        .approve_plan(&env.config, plan.id, &plan.digest, "Operator", WORKERS)
        .unwrap();
    std::fs::remove_file(env.play().join("atari2600/Pong (USA).zip")).unwrap();
    let job = env.store.claim_job().unwrap().unwrap();
    let job = env.store.run_build_job(&env.config, job.id, None).unwrap();
    assert_eq!(job.state, JobState::Failed, "the plan changed");
    env.audit();
    env.record(&everything(&env.sha256));
    assert!(env.store.selection_record(second).unwrap().pruned);
    assert_ne!(held, audit);
    assert!(env.store.audit(held).unwrap().pruned);
}

/// Each entry's name, bytes and mtime, for proving the folder did not change.
fn snapshot(dir: &Path) -> BTreeMap<String, (Option<Vec<u8>>, SystemTime)> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            let meta = std::fs::symlink_metadata(e.path()).unwrap();
            let bytes = meta.is_file().then(|| std::fs::read(e.path()).unwrap());
            (
                e.file_name().to_string_lossy().into_owned(),
                (bytes, meta.modified().unwrap()),
            )
        })
        .collect()
}

fn titles(env: &Env, games: &[&str]) -> i64 {
    env.record(&selection(
        &env.sha256,
        games.iter().map(|g| group(g)).collect(),
    ))
}

fn plan_with(env: &Env, folder: &ManagedFolder, selection: i64, choices: PlanChoices) -> Plan {
    env.store
        .plan_build_with(&env.config, folder.id, &PlanBuild { selection, choices })
        .unwrap()
}

fn check(env: &Env, folder: &ManagedFolder) -> FolderCheck {
    let job = env.store.enqueue_check(folder.id, "Operator").unwrap();
    assert_eq!(job.kind, JobKind::Check);
    assert_eq!(env.store.claim_job().unwrap().unwrap().id, job.id);
    let done = env.store.run_check_job(&env.config, job.id, None).unwrap();
    assert_eq!(done.state, JobState::Succeeded, "{:?}", done.error);
    let check = env.store.latest_check(folder.id).unwrap().unwrap();
    assert_eq!(check.summary.job, Some(job.id));
    check
}

#[test]
fn a_check_reads_only_changed_files_reports_drift_and_never_writes_to_the_folder() {
    let env = Env::new();
    env.audit();
    let id = titles(
        &env,
        &[
            "Pong (USA)",
            "Combat (USA)",
            "Breakout (USA)",
            "Yars (USA)",
            "Missing (USA)",
        ],
    );
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    assert_eq!(env.build(&env.plan(&folder, id)).state, JobState::Succeeded);

    // Combat touched, Breakout rezipped from the same ROMs, Yars edited.
    let combat = std::fs::File::options()
        .write(true)
        .open(dir.join("Combat (USA).zip"))
        .unwrap();
    combat
        .set_modified(SystemTime::now() - Duration::from_secs(3600))
        .unwrap();
    drop(combat);
    write_stored_zip(
        &dir.join("Breakout (USA).zip"),
        &[
            ("Breakout (USA).a26", b"break"),
            ("Breakout (USA).txt", b"manual"),
        ],
    );
    write_stored_zip(&dir.join("Yars (USA).zip"), &[("Yars (USA).a26", b"yarz")]);
    // Drift of every kind.
    write_file(&dir.join("notes.txt"), b"mine");
    torrentzip_file(&dir.join("pong copy.zip"), &[("Pong (USA).a26", b"pong")]);
    torrentzip_file(
        &dir.join("Ōkami (Japan).zip"),
        &[("Ōkami (Japan).a26", b"cafe")],
    );
    torrentzip_file(
        &dir.join("Missing (USA).zip"),
        &[("Missing (USA).a26", b"missing")],
    );
    std::fs::create_dir(dir.join("extras")).unwrap();
    write_file(&dir.join("extras/inside.zip"), b"never read");
    std::os::unix::fs::symlink(env.set_dir().join("Pong (USA).zip"), dir.join("link.zip")).unwrap();
    let before = snapshot(&dir);
    // Unchanged in size and mtime, Pong is never read: a read would fail.
    let pong = dir.join("Pong (USA).zip");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&pong, std::fs::Permissions::from_mode(0o000)).unwrap();

    let first = check(&env, &folder);

    std::fs::set_permissions(&pong, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(snapshot(&dir), before, "a check never writes to the folder");
    let files: BTreeMap<&str, (FileOutcome, bool, bool)> = first
        .files
        .iter()
        .map(|f| (f.game.as_str(), (f.outcome, f.hashed, f.torrentzip)))
        .collect();
    assert_eq!(
        files,
        BTreeMap::from([
            ("Pong (USA)", (FileOutcome::Unchanged, false, true)),
            ("Combat (USA)", (FileOutcome::Refreshed, true, true)),
            ("Breakout (USA)", (FileOutcome::Repacked, true, false)),
            ("Yars (USA)", (FileOutcome::Conflict, true, true)),
        ])
    );
    let drift: BTreeMap<&str, (DriftKind, Option<&str>, bool)> = first
        .drift
        .iter()
        .map(|d| (d.name.as_str(), (d.kind, d.game.as_deref(), d.conflict)))
        .collect();
    assert_eq!(
        drift,
        BTreeMap::from([
            (
                "Missing (USA).zip",
                (DriftKind::Selected, Some("Missing (USA)"), true)
            ),
            ("extras", (DriftKind::Directory, None, false)),
            ("link.zip", (DriftKind::Unknown, None, false)),
            ("notes.txt", (DriftKind::Unknown, None, false)),
            (
                "pong copy.zip",
                (DriftKind::Renamed, Some("Pong (USA)"), false)
            ),
            (
                "Ōkami (Japan).zip",
                (DriftKind::Unselected, Some("Ōkami (Japan)"), false)
            ),
        ])
    );
    assert!(
        first
            .drift
            .iter()
            .all(|d| !d.name.starts_with(".title-select"))
    );

    // The records follow the touched and rezipped files; the rezipped one is not TorrentZip.
    let built = env.store.built_files(folder.id).unwrap();
    let breakout = built.iter().find(|f| f.game == "Breakout (USA)").unwrap();
    assert!(!breakout.torrentzip);
    assert_eq!(
        breakout.file.size,
        std::fs::metadata(dir.join("Breakout (USA).zip"))
            .unwrap()
            .len()
    );
    let again = check(&env, &folder);
    assert!(
        again
            .files
            .iter()
            .filter(|f| f.game != "Yars (USA)")
            .all(|f| f.outcome == FileOutcome::Unchanged && !f.hashed),
        "{:?}",
        again.files
    );

    // Drift is a warning: only the conflicts at selected games' files make the folder incomplete.
    let folder = env.store.managed_folder(&env.config, folder.id).unwrap();
    let counts = folder.incomplete.clone().unwrap();
    assert_eq!(
        (counts.conflict, counts.no_source, counts.total()),
        (1, 1, 2)
    );
    let incomplete = env.store.incomplete(folder.id).unwrap();
    let reasons: BTreeMap<&str, IncompleteReason> = incomplete
        .items
        .iter()
        .map(|i| (i.target.as_str(), i.reason))
        .collect();
    assert_eq!(
        reasons,
        BTreeMap::from([
            ("Missing (USA).zip", IncompleteReason::NoSource),
            ("Yars (USA).zip", IncompleteReason::Conflict),
        ])
    );
    assert!(marker(&dir).get("incomplete").is_none());

    // Planning runs the same check, without a job.
    let plan = env.plan(&folder, id);
    let planned = env.store.latest_check(folder.id).unwrap().unwrap();
    assert_eq!(plan.check, Some(planned.summary.id));
    assert_eq!(planned.summary.job, None);
    assert_eq!(kinds(&plan)["Breakout (USA)"], ItemKind::Unchanged);
    assert_eq!(kinds(&plan)["Yars (USA)"], ItemKind::Conflict);
}

#[test]
fn building_resolved_groups_only_is_a_choice_the_digest_binds_and_leaves_the_rest_incomplete() {
    let env = Env::new();
    env.audit();
    let mut unresolved = everything(&env.sha256);
    unresolved.groups[1].unresolved = true;
    let id = env.record(&unresolved);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    write_file(&dir.join("Combat (USA).zip"), b"theirs");

    let blocked = env.plan(&folder, id);
    assert_eq!(
        blockers(&blocked),
        BTreeSet::from([BlockerKind::Unresolved])
    );
    let partial = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: true,
            adopt: vec![],
        },
    );

    assert!(partial.approvable(), "{:?}", partial.content.blockers);
    assert!(partial.content.choices.resolved_only);
    assert_ne!(partial.digest, blocked.digest);
    assert_eq!(kinds(&partial)["Combat (USA)"], ItemKind::Unresolved);
    let refused = env.store.approve_plan(
        &env.config,
        blocked.id,
        &blocked.digest,
        "Operator",
        WORKERS,
    );
    assert!(matches!(refused, Err(Error::Refused(_))), "{refused:?}");
    assert_eq!(env.build(&partial).state, JobState::Succeeded);

    assert_eq!(
        std::fs::read(dir.join("Combat (USA).zip")).unwrap(),
        b"theirs"
    );
    assert!(names(&dir).contains("Pong (USA).zip"));
    let folder = env.store.managed_folder(&env.config, folder.id).unwrap();
    let counts = folder.incomplete.clone().unwrap();
    assert_eq!(
        (
            counts.unresolved,
            counts.no_source,
            counts.blocked,
            counts.failed,
            counts.conflict
        ),
        (1, 1, 1, 0, 0)
    );
    let incomplete = env.store.incomplete(folder.id).unwrap();
    let combat = incomplete
        .items
        .iter()
        .find(|i| i.reason == IncompleteReason::Unresolved)
        .unwrap();
    assert_eq!(
        (combat.group.as_str(), combat.target.as_str()),
        ("combat (usa)", "Combat (USA).zip")
    );
    assert!(marker(&dir).get("incomplete").is_none());
}

#[test]
fn a_conflict_is_adopted_only_when_its_roms_match_the_dat_and_nothing_is_written() {
    let env = Env::new();
    env.audit();
    let id = titles(&env, &["Pong (USA)", "Combat (USA)"]);
    let folder = env.folder("atari2600");
    let dir = env.play().join("atari2600");
    write_stored_zip(&dir.join("Pong (USA).zip"), &[("Pong (USA).a26", b"pong")]);
    write_stored_zip(
        &dir.join("Combat (USA).zip"),
        &[("Combat (USA).a26", b"combat!")],
    );
    let before = snapshot(&dir);

    let left = env.plan(&folder, id);
    let adoptable: BTreeMap<&str, (ItemKind, bool)> = left
        .content
        .items
        .iter()
        .map(|i| (i.game.as_str(), (i.kind, i.adoptable)))
        .collect();
    assert_eq!(
        adoptable,
        BTreeMap::from([
            ("Combat (USA)", (ItemKind::Conflict, false)),
            ("Pong (USA)", (ItemKind::Conflict, true)),
        ])
    );
    let adopt = plan_with(
        &env,
        &folder,
        id,
        PlanChoices {
            resolved_only: false,
            adopt: vec!["Pong (USA).zip".into(), "Combat (USA).zip".into()],
        },
    );
    assert_ne!(adopt.digest, left.digest);
    assert_eq!(kinds(&adopt)["Pong (USA)"], ItemKind::Adopt);
    assert_eq!(kinds(&adopt)["Combat (USA)"], ItemKind::Conflict);
    assert_eq!(adopt.summary.adopt, 1);

    assert_eq!(env.build(&adopt).state, JobState::Succeeded);

    let after = snapshot(&dir);
    assert_eq!(after.len(), before.len());
    for (name, entry) in &before {
        if name != ".title-select" {
            assert_eq!(&after[name], entry, "{name}");
        }
    }
    let built = env.store.built_files(folder.id).unwrap();
    assert_eq!(built.len(), 1);
    assert_eq!(
        (built[0].kind, built[0].torrentzip),
        (ItemKind::Adopt, false)
    );
    let folder = env.store.managed_folder(&env.config, folder.id).unwrap();
    assert_eq!(folder.incomplete.as_ref().unwrap().conflict, 1);
    // Adopted, it is the folder's own: planned again, it is unchanged.
    assert_eq!(
        kinds(&env.plan(&folder, id))["Pong (USA)"],
        ItemKind::Unchanged
    );
}

#[test]
fn an_interrupted_build_is_badged_from_the_store_and_a_new_plan_completes_the_folder() {
    let env = Env::new();
    env.audit();
    let id = titles(
        &env,
        &["Pong (USA)", "Combat (USA)", "Breakout (USA)", "Yars (USA)"],
    );
    let folder = env.folder("atari2600");
    let plan = env.plan(&folder, id);
    env.crash(&plan, Fault::AfterPublish(2));
    let (env, _) = env.restart();

    let badged = env.store.managed_folder(&env.config, folder.id).unwrap();
    let unfinished = badged.unfinished.clone().unwrap();
    assert_eq!(unfinished.state, BuildState::Interrupted);
    let detail = env.store.build(unfinished.build).unwrap();
    let count = |s: ItemState| detail.items.iter().filter(|i| i.state == s).count() as u64;
    assert_eq!(
        (unfinished.applied, unfinished.uncertain),
        (count(ItemState::Applied), count(ItemState::Uncertain))
    );
    assert!(unfinished.applied + unfinished.uncertain > 0);

    let next = env.plan(&badged, id);
    assert!(next.approvable(), "{:?}", next.content.blockers);
    assert_eq!(env.build(&next).state, JobState::Succeeded);

    let done = env.store.managed_folder(&env.config, folder.id).unwrap();
    assert_eq!(done.unfinished, None);
    assert_eq!(done.incomplete, None);
    assert_eq!(done.files, 4);
}
