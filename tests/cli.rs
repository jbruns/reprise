mod common;

use std::process::Command;

use common::*;

fn title_select() -> Command {
    Command::new(env!("CARGO_BIN_EXE_title-select"))
}

fn pong_fixture() -> (Fixture, std::path::PathBuf) {
    let f = Fixture::new();
    let dat = f.dat(&[("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")])]);
    write_file(&f.set_path("Pong (USA).a26"), b"pong");
    (f, dat)
}

#[test]
fn audit_prints_a_json_report_to_stdout() {
    let (f, dat) = pong_fixture();

    let out = title_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], 2);
    assert_eq!(report["dat_revision"]["name"], "Test - System");
    assert_eq!(report["summary"]["findings"]["matched"], 1);
    assert_eq!(report["entries"][0]["finding"], "matched");
    assert_eq!(report["entries"][0]["evidence"]["level"], "payload");
    assert_eq!(report["games"][0]["status"], "complete");
}

#[test]
fn audit_writes_the_report_to_the_output_file() {
    let (f, dat) = pong_fixture();
    let output = f.dat_dir.path().join("report.json");

    let out = title_select()
        .arg("audit")
        .arg("--dat")
        .arg(&dat)
        .arg(f.set.path())
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stdout.is_empty());
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(report["summary"]["findings"]["matched"], 1);
}

#[test]
fn audit_fails_with_a_message_for_an_unreadable_dat_or_a_missing_source_set() {
    let (f, dat) = pong_fixture();
    let not_xml = f.dat_dir.path().join("not.dat");
    std::fs::write(&not_xml, "<datafile><game name=\"x\"></datafile>").unwrap();
    let missing_set = f.dat_dir.path().join("no-such-set");

    for (dat, set) in [
        (not_xml.as_path(), f.set.path()),
        (dat.as_path(), missing_set.as_path()),
    ] {
        let out = title_select()
            .arg("audit")
            .arg("--dat")
            .arg(dat)
            .arg(set)
            .output()
            .unwrap();

        assert!(!out.status.success());
        assert!(out.stdout.is_empty());
        assert!(!out.stderr.is_empty());
    }
}

fn run_ok(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_registered_source_set_is_audited_into_the_store_and_listed_in_its_history() {
    let (f, dat) = pong_fixture();
    let config = f.dat_dir.path().join("title-select.toml");
    let root = f.set.path().parent().unwrap();
    let rel = f.set.path().file_name().unwrap().to_str().unwrap();
    std::fs::write(
        &config,
        format!("[roots.sets]\npath = {:?}\n", root.display().to_string()),
    )
    .unwrap();
    let store = f.dat_dir.path().join("store");
    let ts = || {
        let mut cmd = title_select();
        cmd.env("TITLE_SELECT_STORE", &store)
            .env("TITLE_SELECT_CONFIG", &config);
        cmd
    };

    let imported = run_ok(
        ts().args(["dat", "import"])
            .arg(&dat)
            .args(["--provider", "Test"]),
    );
    assert!(imported.contains("Test/Test - System"), "{imported}");
    run_ok(ts().args(["set", "add", "sets", rel, "--dat", "Test/Test - System"]));
    let report: serde_json::Value =
        serde_json::from_str(&run_ok(ts().args(["audit", "--set", rel]))).unwrap();
    assert_eq!(report["schema"], 2);
    assert_eq!(report["summary"]["findings"]["matched"], 1);

    let sets = run_ok(ts().args(["set", "list"]));
    let sha = report["dat_revision"]["sha256"].as_str().unwrap();
    assert!(sets.starts_with(rel), "{sets}");
    assert!(
        sets.contains(&format!("last verified against {sha}")),
        "{sets}"
    );
    let history = run_ok(ts().args(["set", "history", rel]));
    assert_eq!(history.lines().count(), 1);
    assert!(
        history.contains("matched=1") && history.contains("complete=1"),
        "{history}"
    );
}

#[test]
fn a_stored_audit_without_a_store_fails_with_a_message() {
    let out = title_select()
        .env_remove("TITLE_SELECT_STORE")
        .args(["audit", "--set", "Anything"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--store"));
}
