#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::*;
use title_select_server::{Handle, Settings};
use title_select_store::Store;
use title_select_store::config::Config;

const SET: &str = "Test - System";
const TOKEN: &str = "headless test token";

/// A Service with one registered Source Set, run in this process, and the files the headless client is given.
struct Env {
    fx: Fixture,
    service: Handle,
}

impl Env {
    fn new() -> Env {
        let fx = Fixture::new();
        let set = fx.set_path(SET);
        write_file(&set.join("Pong (USA).a26"), b"pong");
        write_file(&set.join("stray.bin"), b"stray");
        let dat = fx.dat(&[
            ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
            ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
        ]);
        let config = Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n",
            fx.set.path().display().to_string()
        ))
        .unwrap();
        let store = fx.dat_dir.path().join("store");
        {
            let store = Store::open(&store).unwrap();
            store.import_dat(&dat, "No-Intro", None).unwrap();
            store
                .add_source_set(&config, "sets", SET, "No-Intro/Test - System")
                .unwrap();
        }
        std::fs::write(fx.dat_dir.path().join("token"), TOKEN).unwrap();
        std::fs::write(fx.dat_dir.path().join("wrong"), "not the token").unwrap();
        let service = title_select_server::spawn(Settings {
            store,
            config,
            listen: "127.0.0.1:0".parse().unwrap(),
            token_file: Some(fx.dat_dir.path().join("token")),
            hold_jobs: false,
        })
        .unwrap();
        Env { fx, service }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.fx.dat_dir.path().join(name)
    }

    fn audit(&self, server: &str, token: &Path, extra: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_title-select"))
            .args(["audit", "--server", server, "--set", SET])
            .arg("--token-file")
            .arg(token)
            .args(extra)
            .env_remove("TITLE_SELECT_STORE")
            .output()
            .unwrap()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn the_headless_client_runs_an_audit_job_and_prints_the_report() {
    let env = Env::new();

    let out = env.audit(&env.service.url(), &env.file("token"), &[]);

    assert!(out.status.success(), "{}", stderr(&out));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["schema"], 2);
    assert!(report["source_set"].as_str().unwrap().ends_with(SET));
    assert_eq!(report["summary"]["findings"]["matched"], 1);
    assert_eq!(report["summary"]["findings"]["unmatched"], 1);
    let err = stderr(&out);
    assert!(err.contains("succeeded"), "{err}");
    assert!(err.contains("matched=1"), "{err}");

    let output = env.file("report.json");
    let out = env.audit(
        &env.service.url(),
        &env.file("token"),
        &["--output", output.to_str().unwrap()],
    );
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(out.stdout.is_empty());
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(written["summary"], report["summary"]);
}

#[test]
fn the_headless_client_fails_on_a_wrong_token_an_unreachable_server_or_a_failed_job() {
    let env = Env::new();

    let wrong = env.audit(&env.service.url(), &env.file("wrong"), &[]);
    assert!(!wrong.status.success());
    assert!(
        stderr(&wrong).contains("Operator token"),
        "{}",
        stderr(&wrong)
    );

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let closed = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let unreachable = env.audit(&closed, &env.file("token"), &[]);
    assert!(!unreachable.status.success());
    assert!(
        stderr(&unreachable).contains("cannot reach"),
        "{}",
        stderr(&unreachable)
    );

    std::fs::remove_dir_all(env.fx.set_path(SET)).unwrap();
    let failed = env.audit(&env.service.url(), &env.file("token"), &[]);
    assert!(!failed.status.success());
    assert!(stderr(&failed).contains("failed"), "{}", stderr(&failed));
    assert!(failed.stdout.is_empty());
}
