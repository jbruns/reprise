//! The Selection engine's contract beyond conformance: unpatched and patched Retool keep the same titles, a
//! fail-safe tie is flagged, a Selection never fetches, and a Profile Retool can't read is refused.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use title_select_api::selection::SelectionProfile;
use title_select_selection::{Engine, Error};

/// Retool's synthetic DATs and settings, as Retool's own tests run them; `@name` is `tests/configs/user-config-name.yaml`. Retool writes the same output DATs
/// whether or not it is patched.
const NEUTRALITY_CASES: &[(&str, &str)] = &[
    ("Retool - Clone list filters.dat", "@regions-1"),
    ("Retool - Clone list filters.dat", "@clone-list-filters-1"),
    ("Retool - Clone list filters.dat", "@clone-list-filters-2"),
    ("Retool - Clone list filters.dat", "@clone-list-filters-3"),
    ("Retool - Compilations.dat", "@regions-1"),
    ("Retool - Compilations.dat", "@regions-1 --compilations i"),
    ("Retool - Compilations.dat", "@regions-1 --compilations k"),
    ("Retool - Compilations.dat", "@regions-1 --compilations o"),
    (
        "Retool - Exclusions.dat",
        "@regions-1 --exclude aAbBcdDefkmMopPruv",
    ),
    ("Retool - Languages.dat", "@languages-1 -l"),
    ("Retool - Languages.dat", "@languages-2 -l"),
    ("Retool - Languages.dat", "@languages-3 -l"),
    ("Retool - Local names.dat", "@local-names -n"),
    ("Retool - Machine and disk.dat", "@regions-1"),
    ("Retool - MIAs.dat", "@regions-1 --labelmia"),
    ("Retool - MIAs.dat", "@regions-1 --exclude k"),
    ("Retool - Missing header data.dat", "@regions-1"),
    ("Retool - Missing title data.dat", "@regions-1"),
    ("Retool - No size or hash.dat", "@regions-1"),
    ("Retool - Numbered.dat", "@regions-1"),
    ("Retool - Post filters.dat", "@post-filters"),
    ("Retool - Prefer licensed.dat", "@regions-1 -y"),
    ("Retool - Prefer modern rips.dat", "@regions-1 -z"),
    ("Retool - Prefer oldest.dat", "@regions-1 -o"),
    ("Retool - Prefer oldest.dat", "@regions-1"),
    ("Retool - Prefer regions.dat", "@regions-1 -r"),
    ("Retool - RetroAchievements.dat", "@regions-1 --labelretro"),
    ("Retool - RetroAchievements.dat", "@regions-1 -c"),
    ("Retool - World is USA-Europe-Japan.dat", "@regions-1"),
];

fn run_retool(engine: &Engine, retool: &Path, dat: &str, args: &str, output: &Path) {
    let mut command = Command::new(engine.python());
    command
        .arg(retool.join("retool.py"))
        .arg(Path::new("tests/source/features").join(dat))
        .args(["--test", "--output"])
        .arg(output);
    for word in args.split_whitespace() {
        match word.strip_prefix('@') {
            Some(config) => command
                .arg("--config")
                .arg(format!("tests/configs/user-config-{config}.yaml")),
            None => command.arg(word),
        };
    }
    let result = command
        .current_dir(support::upstream())
        .env("PYTHONHASHSEED", "0")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("https_proxy", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {dat} {args}: {}\n{}",
        retool.display(),
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Every file under `dir`, relative to it, with its contents.
fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let contents = std::fs::read(&path).unwrap();
                found.push((path.strip_prefix(dir).unwrap().to_path_buf(), contents));
            }
        }
    }
    found.sort();
    found
}

#[test]
fn the_patches_leave_retools_output_unchanged() {
    let engine = support::engine();
    let pristine = engine
        .pristine()
        .expect("the install has no pristine Retool: run tools/retool/setup");
    let patched = pristine.parent().unwrap().join("retool");
    let work = tempfile::tempdir().unwrap();
    let next = Mutex::new(0);
    let failures = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                loop {
                    let index = {
                        let mut next = next.lock().unwrap();
                        *next += 1;
                        *next - 1
                    };
                    let Some((dat, args)) = NEUTRALITY_CASES.get(index) else {
                        break;
                    };
                    let outputs = ["pristine", "patched"]
                        .map(|name| work.path().join(index.to_string()).join(name));
                    run_retool(&engine, &pristine, dat, args, &outputs[0]);
                    run_retool(&engine, &patched, dat, args, &outputs[1]);
                    let [before, after] = outputs.map(|output| files(&output));
                    if before.is_empty() || before != after {
                        failures.lock().unwrap().push(format!("{dat} {args}"));
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().unwrap();
    assert!(
        failures.is_empty(),
        "the patched Retool writes other output than the pristine one for: {failures:#?}"
    );
}

fn rom(name: &str) -> String {
    format!(
        "\t<game name=\"{name}\">\n\t\t<category>Games</category>\n\t\t<description>{name}</description>\n\
         \t\t<rom name=\"{name}.bin\" size=\"1000\" crc=\"00000000\" md5=\"00000000000000000000000000000000\" \
         sha1=\"0000000000000000000000000000000000000000\"/>\n\t</game>\n"
    )
}

fn dat(path: &Path, name: &str, titles: &[&str]) {
    let games: String = titles.iter().map(|title| rom(title)).collect();
    std::fs::write(
        path,
        format!(
            "<?xml version=\"1.0\"?>\n<datafile>\n\t<header>\n\t\t<name>{name}</name>\n\
             \t\t<description>{name}</description>\n\t\t<version>2024-01-01 00-00-00</version>\n\
             \t</header>\n{games}</datafile>\n"
        ),
    )
    .unwrap();
}

/// An empty data folder: no clone lists, metadata, MIAs or RetroAchievements.
fn data_dir(root: &Path) -> PathBuf {
    let data = root.join("data");
    for dir in ["clonelists", "metadata", "mias", "retroachievements"] {
        std::fs::create_dir_all(data.join(dir)).unwrap();
    }
    data
}

#[test]
fn a_fail_safe_tie_is_unresolved_and_keeps_retools_pick() {
    let work = tempfile::tempdir().unwrap();
    let data = data_dir(work.path());
    let name = "Title Select - Fail-safe tie";
    std::fs::write(
        data.join("clonelists").join(format!("{name}.json")),
        r#"{
  "description": {"name": "Title Select - Fail-safe tie", "lastUpdated": "2024-01-01 00:00:00", "minimumVersion": "2.4.8"},
  "variants": [
    {"group": "Tie Alpha", "titles": [{"searchTerm": "Tie Alpha"}, {"searchTerm": "Tie Beta"}]}
  ]
}
"#,
    )
    .unwrap();
    let path = work.path().join("tie.dat");
    dat(
        &path,
        name,
        &["Tie Alpha (Europe)", "Tie Beta (Europe)", "Solo (Europe)"],
    );

    let selection = support::engine()
        .with_data_dir(&data)
        .select(
            &path,
            &SelectionProfile::default(),
            &support::options(work.path()),
        )
        .unwrap();

    let tie = selection
        .groups
        .iter()
        .find(|g| g.group == "tie alpha")
        .unwrap_or_else(|| panic!("no tie group in {:#?}", selection.groups));
    assert!(tie.unresolved, "{tie:#?}");
    assert_eq!(tie.winners.len(), 1, "{tie:#?}");
    assert!(tie.winners[0].unresolved);
    assert_eq!(tie.winners[0].rule.reference.as_deref(), Some("REF0059"));
    assert_eq!(tie.alternatives.len(), 1, "{tie:#?}");

    let solo = selection.groups.iter().find(|g| g.group == "solo").unwrap();
    assert!(!solo.unresolved, "{solo:#?}");
    assert_eq!(selection.summary.unresolved, 1);
}

#[test]
fn a_selection_without_clone_lists_fails_instead_of_fetching() {
    let work = tempfile::tempdir().unwrap();
    let empty = work.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - No data", &["Solo (Europe)"]);

    let error = support::engine()
        .with_data_dir(&empty)
        .select(
            &path,
            &SelectionProfile::default(),
            &support::options(work.path()),
        )
        .unwrap_err();

    match error {
        Error::Retool { output, .. } => assert!(output.contains("clonelists"), "{output}"),
        other => panic!("expected Retool to fail, got {other}"),
    }
}

#[test]
fn a_profile_with_names_retool_does_not_know_is_refused() {
    let work = tempfile::tempdir().unwrap();
    let path = work.path().join("solo.dat");
    dat(&path, "Title Select - Profile", &["Solo (Europe)"]);
    let engine = support::engine();
    let refused = |profile: SelectionProfile| match engine.select(
        &path,
        &profile,
        &support::options(work.path()),
    ) {
        Err(Error::Profile(message)) => message,
        other => panic!("expected the Profile to be refused, got {other:?}"),
    };

    let profile = SelectionProfile {
        region_order: vec!["Atlantis".into()],
        ..Default::default()
    };
    assert!(refused(profile).contains("Atlantis"));

    let profile = SelectionProfile {
        language_order: vec!["Klingon".into()],
        ..Default::default()
    };
    assert!(refused(profile).contains("Klingon"));

    let mut profile = SelectionProfile::default();
    profile
        .systems
        .insert("../escape".into(), Default::default());
    assert!(refused(profile).contains("escape"));
}

#[test]
fn an_install_built_from_other_pins_is_refused() {
    let work = tempfile::tempdir().unwrap();
    std::fs::write(
        work.path().join("pins.json"),
        r#"{"retool_commit": "0", "patch_set_sha256": "0", "clone_list_commit": "0"}"#,
    )
    .unwrap();
    match Engine::open(work.path()) {
        Err(Error::Install(message)) => assert!(message.contains("pins.toml"), "{message}"),
        other => panic!("expected the install to be refused, got {other:?}"),
    }
}

/// The acceptance run on a real DAT, which can't be committed:
/// `TITLE_SELECT_ATARI_DAT=<path> cargo test -p title-select-selection --test selection -- --ignored --nocapture`.
#[test]
#[ignore = "needs a real DAT in TITLE_SELECT_ATARI_DAT"]
fn a_real_dat_gets_a_rule_for_every_group() {
    let path = std::env::var_os("TITLE_SELECT_ATARI_DAT").expect("set TITLE_SELECT_ATARI_DAT");
    let work = tempfile::tempdir().unwrap();
    let started = std::time::Instant::now();
    let selection = support::engine()
        .select(
            Path::new(&path),
            &SelectionProfile::default(),
            &support::options(work.path()),
        )
        .unwrap();
    let elapsed = started.elapsed();

    for group in &selection.groups {
        for winner in &group.winners {
            assert!(
                winner.rule.reference.is_some(),
                "{}: {group:#?}",
                winner.title
            );
        }
        for alternative in &group.alternatives {
            assert!(!alternative.rule.message.is_empty(), "{group:#?}");
        }
    }
    let unresolved: Vec<&str> = selection
        .groups
        .iter()
        .filter(|g| g.unresolved)
        .map(|g| g.group.as_str())
        .collect();
    println!(
        "{}: {:?} in {elapsed:.2?}\nunresolved: {unresolved:?}\ncoverage drift: clone list {}, metadata {}, \
         {} unmatched clone-list terms, {} titles without metadata",
        selection.system,
        selection.summary,
        selection.coverage_drift.clone_list,
        selection.coverage_drift.metadata,
        selection.coverage_drift.unmatched_clone_list_terms.len(),
        selection.coverage_drift.titles_without_metadata.len(),
    );
}
