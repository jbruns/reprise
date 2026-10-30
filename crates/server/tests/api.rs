#[path = "../../engine/tests/common/mod.rs"]
mod common;

use std::io::BufRead;
use std::path::PathBuf;

use common::*;
use serde::de::DeserializeOwned;
use title_select_api::{
    AuditSummary, EntryRow, ErrorBody, Finding, GameRow, ImportOutcome, ImportedDatRevision, Job,
    JobEvent, JobState, Page, Report, SourceSetRecord,
};
use title_select_server::{Handle, Settings, StartError, TOKEN_ACTOR};
use title_select_store::config::Config;

const TOKEN: &str = "correct horse battery staple";
const SET: &str = "Test - System";

/// A store directory, a Root called `sets` holding one Source Set folder, a DAT to upload and a token file.
struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Env {
        let tmp = tempfile::tempdir().unwrap();
        let set = tmp.path().join("roots/sets").join(SET);
        std::fs::create_dir_all(&set).unwrap();
        write_file(&set.join("Pong (USA).a26"), b"pong");
        write_zip(
            &set.join("Combat (USA).zip"),
            &[("Combat (USA).a26", b"combat")],
        );
        write_file(&set.join("notes.txt"), b"not a rom");
        std::fs::create_dir_all(tmp.path().join("in")).unwrap();
        std::fs::write(tmp.path().join("token"), format!("{TOKEN}\n")).unwrap();
        Env { tmp }
    }

    fn store(&self) -> PathBuf {
        self.tmp.path().join("store")
    }

    fn config(&self, extra: &str) -> Config {
        let root = self.tmp.path().join("roots/sets");
        Config::parse(&format!(
            "[roots.sets]\npath = {:?}\n{extra}",
            root.display().to_string()
        ))
        .unwrap()
    }

    fn settings(&self, extra: &str) -> Settings {
        Settings {
            store: self.store(),
            config: self.config(extra),
            listen: "127.0.0.1:0".parse().unwrap(),
            token_file: Some(self.tmp.path().join("token")),
            ui: None,
            retool: None,
            hold_jobs: false,
        }
    }

    fn start(&self) -> Handle {
        title_select_server::spawn(self.settings("")).unwrap()
    }

    fn dat(&self) -> Vec<u8> {
        let path = write_dat_version(
            &self.tmp.path().join("in"),
            "Test - System (20260101).dat",
            "20260101-000000",
            &[
                ("Pong (USA)", vec![rom("Pong (USA).a26", b"pong")]),
                ("Combat (USA)", vec![rom("Combat (USA).a26", b"combat")]),
                ("Breakout (USA)", vec![rom("Breakout (USA).a26", b"break")]),
            ],
        );
        std::fs::read(path).unwrap()
    }
}

fn bearer(token: &str) -> Option<(String, String)> {
    Some(("Authorization".into(), format!("Bearer {token}")))
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into()
}

/// An authenticated client of one Service.
struct Client {
    base: String,
    /// A header to send with every request, and its value.
    auth: Option<(String, String)>,
}

impl Client {
    fn new(service: &Handle) -> Client {
        Client {
            base: format!("{}/api/v1", service.url()),
            auth: bearer(TOKEN),
        }
    }

    fn get(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        let request = agent().get(format!("{}{path}", self.base));
        match &self.auth {
            Some((name, value)) => request.header(name, value),
            None => request,
        }
    }

    fn post(&self, path: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        let request = agent().post(format!("{}{path}", self.base));
        match &self.auth {
            Some((name, value)) => request.header(name, value),
            None => request,
        }
    }

    fn json<T: DeserializeOwned>(&self, path: &str) -> T {
        let mut response = self.get(path).call().unwrap();
        assert_eq!(response.status(), 200, "GET {path}");
        response.body_mut().read_json().unwrap()
    }

    fn upload(&self, file: &str, bytes: &[u8]) -> (u16, ImportedDatRevision) {
        let boundary = "title-select-test-boundary";
        let mut body = Vec::new();
        for (name, value) in [("provider", "No-Intro"), ("source_url", "")] {
            body.extend(format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            ).bytes());
        }
        body.extend(format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{file}\"\r\n\
             Content-Type: application/xml\r\n\r\n"
        ).bytes());
        body.extend(bytes);
        body.extend(format!("\r\n--{boundary}--\r\n").bytes());
        let mut response = self
            .post("/dat-revisions")
            .header(
                "Content-Type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .send(&body[..])
            .unwrap();
        let status = response.status().as_u16();
        (status, response.body_mut().read_json().unwrap())
    }

    fn add_set(&self) -> SourceSetRecord {
        let mut response = self
            .post("/source-sets")
            .send_json(
                serde_json::json!({"root": "sets", "path": SET, "dat": "No-Intro/Test - System"}),
            )
            .unwrap();
        assert_eq!(response.status(), 201);
        response.body_mut().read_json().unwrap()
    }

    fn start_audit(&self) -> ureq::http::Response<ureq::Body> {
        self.post("/audits")
            .send_json(serde_json::json!({"source_set": SET}))
            .unwrap()
    }

    /// Subscribes to every Job's events, returning once the `resync` sent on connecting has arrived, with its Jobs.
    fn watch(&self) -> (Events, title_select_api::JobSnapshot) {
        let response = self.get("/events").call().unwrap();
        assert_eq!(response.status(), 200);
        let mut events = Events(Box::new(
            std::io::BufReader::new(response.into_body().into_reader()).lines(),
        ));
        let (name, data) = events.next_event();
        assert_eq!(name, "resync");
        (events, serde_json::from_str(&data).unwrap())
    }

    /// Follows the Job's events to its final state, returning the event names and the last Job.
    fn follow(&self, id: i64) -> (Vec<String>, Job) {
        let response = self.get(&format!("/jobs/{id}/events")).call().unwrap();
        assert_eq!(response.status(), 200);
        let reader = std::io::BufReader::new(response.into_body().into_reader());
        let (mut names, mut name, mut last) = (Vec::new(), String::new(), None);
        for line in reader.lines() {
            let line = line.unwrap();
            if let Some(n) = line.strip_prefix("event: ") {
                name = n.to_owned();
            } else if let Some(data) = line.strip_prefix("data: ") {
                names.push(name.clone());
                if name == "state" {
                    last = Some(serde_json::from_str::<Job>(data).unwrap());
                }
            }
        }
        (names, last.expect("a state event"))
    }
}

/// A stream of server-sent events.
struct Events(Box<dyn Iterator<Item = std::io::Result<String>>>);

impl Events {
    /// The next event's name and data.
    fn next_event(&mut self) -> (String, String) {
        let mut name = String::new();
        for line in &mut self.0 {
            let line = line.unwrap();
            if let Some(n) = line.strip_prefix("event: ") {
                name = n.to_owned();
            } else if let Some(data) = line.strip_prefix("data: ") {
                return (name, data.to_owned());
            }
        }
        panic!("the event stream ended");
    }

    /// The names of the Job `id`'s events until its final state, and the Job then.
    fn follow(&mut self, id: i64) -> (Vec<String>, Job) {
        let mut names = Vec::new();
        loop {
            let (name, data) = self.next_event();
            let event: JobEvent = serde_json::from_str(&data).unwrap();
            if event.job_id() != Some(id) {
                continue;
            }
            names.push(name);
            if let JobEvent::State(job) = event
                && job.state.is_final()
            {
                return (names, *job);
            }
        }
    }
}

fn status_of(response: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> u16 {
    response.unwrap().status().as_u16()
}

#[test]
fn startup_is_refused_with_no_auth_configured() {
    let env = Env::new();
    let settings = Settings {
        token_file: None,
        ..env.settings("")
    };

    let refused = title_select_server::spawn(settings);

    assert!(
        matches!(refused, Err(StartError::Auth(_))),
        "{:?}",
        refused.err()
    );
    assert!(
        !env.store().exists(),
        "nothing is opened before auth is checked"
    );
}

#[test]
fn startup_is_refused_with_dev_identity_off_loopback() {
    let env = Env::new();
    let settings = Settings {
        listen: "0.0.0.0:0".parse().unwrap(),
        ..env.settings("[auth]\ndev_identity = \"Developer\"\n")
    };

    let refused = title_select_server::spawn(settings);

    let Err(StartError::Auth(message)) = refused else {
        panic!("expected a refusal");
    };
    assert!(message.contains("loopback"), "{message}");
}

#[test]
fn dev_identity_on_loopback_authenticates_requests_with_no_credential() {
    let env = Env::new();
    let settings = Settings {
        token_file: None,
        ..env.settings("[auth]\ndev_identity = \"Developer\"\n")
    };
    let service = title_select_server::spawn(settings).unwrap();
    let client = Client {
        auth: None,
        ..Client::new(&service)
    };

    assert_eq!(status_of(client.get("/jobs").call()), 200);
    let wrong = Client {
        auth: bearer("nope"),
        ..Client::new(&service)
    };
    assert_eq!(status_of(wrong.get("/jobs").call()), 401);
}

#[test]
fn the_workbench_is_served_beside_the_api_without_hiding_its_errors() {
    let env = Env::new();
    let ui = env.tmp.path().join("site");
    std::fs::create_dir_all(ui.join("pkg")).unwrap();
    std::fs::write(
        ui.join("workbench.html"),
        "<!doctype html><title>workbench</title>",
    )
    .unwrap();
    std::fs::write(ui.join("pkg/title-select.js"), "export default 1;").unwrap();
    let service = title_select_server::spawn(Settings {
        ui: Some(ui),
        ..env.settings("")
    })
    .unwrap();
    let page = |path: &str| {
        let mut response = agent()
            .get(format!("{}{path}", service.url()))
            .call()
            .unwrap();
        let csp = response.headers().get("content-security-policy").cloned();
        (
            response.status().as_u16(),
            response.body_mut().read_to_string().unwrap(),
            csp,
        )
    };

    let (status, body, csp) = page("/");
    assert_eq!(status, 200);
    assert!(body.contains("workbench"));
    assert!(
        csp.unwrap()
            .to_str()
            .unwrap()
            .starts_with("default-src 'self'")
    );
    let (status, body, _) = page("/audits/7");
    assert_eq!(
        (status, body.contains("workbench")),
        (200, true),
        "a screen's URL loads the workbench"
    );
    assert_eq!(page("/pkg/title-select.js").1, "export default 1;");

    for path in ["/api/v1/no-such-thing", "/api/v2/jobs", "/api"] {
        let (status, body, _) = page(path);
        assert_eq!(status, 404, "{path}");
        let error: ErrorBody = serde_json::from_str(&body).unwrap();
        assert_eq!(error.error, "no such API path");
    }
    assert_eq!(
        page("/api/v1/jobs").0,
        401,
        "the API still needs a credential"
    );
}

#[test]
fn a_missing_or_wrong_token_is_rejected_and_the_correct_one_accepted() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);

    let missing = Client {
        auth: None,
        ..Client::new(&service)
    };
    let response = missing.get("/source-sets").call().unwrap();
    assert_eq!(response.status(), 401);
    assert_eq!(response.headers()["WWW-Authenticate"], "Bearer");
    for wrong in [
        "Bearer correct horse",
        "Bearer ",
        "Basic Zm9vOmJhcg==",
        TOKEN,
    ] {
        let wrong = Client {
            auth: Some(("Authorization".into(), wrong.into())),
            ..Client::new(&service)
        };
        assert_eq!(
            status_of(wrong.get("/source-sets").call()),
            401,
            "{:?}",
            wrong.auth
        );
    }

    assert_eq!(status_of(client.get("/source-sets").call()), 200);
    assert_eq!(
        status_of(missing.get("/health").call()),
        200,
        "health is unauthenticated"
    );
}

#[test]
fn an_identity_header_is_trusted_only_from_a_trusted_proxy() {
    let env = Env::new();
    let header = "[auth]\nidentity_header = \"Remote-User\"\n";
    let untrusted = title_select_server::spawn(Settings {
        token_file: None,
        ..env.settings(&format!("{header}trusted_proxies = [\"192.0.2.0/24\"]\n"))
    })
    .unwrap();
    let client = Client {
        auth: None,
        ..Client::new(&untrusted)
    };

    let response = client.get("/jobs").header("Remote-User", "Operator").call();

    assert_eq!(status_of(response), 401);
    assert_eq!(status_of(client.get("/jobs").call()), 401);
    drop(untrusted);

    let trusted = title_select_server::spawn(Settings {
        token_file: None,
        ..env.settings(&format!(
            "{header}trusted_proxies = [\"127.0.0.0/8\", \"::1/128\"]\n"
        ))
    })
    .unwrap();
    let client = Client {
        auth: Some(("Remote-User".into(), "the Operator".into())),
        ..Client::new(&trusted)
    };
    client.upload("Test - System.dat", &env.dat());
    client.add_set();

    let mut response = client.start_audit();

    assert_eq!(response.status(), 202);
    let job: Job = response.body_mut().read_json().unwrap();
    assert_eq!(job.actor, "the Operator");
}

#[test]
fn the_api_uploads_a_dat_registers_a_source_set_and_audits_it_end_to_end() {
    let env = Env::new();
    let service = env.start();
    let client = Client::new(&service);
    let dat = env.dat();

    let (status, uploaded) = client.upload("Test - System (20260101).dat", &dat);
    assert_eq!((status, uploaded.outcome), (201, ImportOutcome::Added));
    assert_eq!(uploaded.dat_revision.version, "20260101-000000");
    assert_eq!(uploaded.dat_revision.file, "Test - System (20260101).dat");
    let (status, again) = client.upload("renamed.dat", &dat);
    assert_eq!(
        (status, again.outcome),
        (200, ImportOutcome::AlreadyImported)
    );
    let dats: Vec<title_select_api::Dat> = client.json("/dats");
    assert_eq!(dats.len(), 1);
    assert_eq!(dats[0].dat, "No-Intro/Test - System");
    let roots: Vec<title_select_api::Root> = client.json("/roots");
    assert_eq!(roots[0].name, "sets");

    let set = client.add_set();
    assert_eq!(set.name, SET);
    let fetched: SourceSetRecord = client.json(&format!("/source-sets/{}", "Test%20-%20System"));
    assert_eq!(fetched, set);

    let (mut watching, snapshot) = client.watch();
    assert!(snapshot.jobs.is_empty());
    let mut response = client.start_audit();
    assert_eq!(response.status(), 202);
    let job: Job = response.body_mut().read_json().unwrap();
    assert_eq!(
        (job.actor.as_str(), job.dat_revision.as_str()),
        (TOKEN_ACTOR, uploaded.dat_revision.sha256.as_str())
    );

    let (events, done) = watching.follow(job.id);
    assert_eq!(done.state, JobState::Succeeded, "{done:?}");
    assert!(events.contains(&"progress".to_string()), "{events:?}");
    let (_, followed) = client.follow(job.id);
    assert_eq!(
        followed, done,
        "a finished Job's stream sends its final state"
    );
    let (_, snapshot) = client.watch();
    assert_eq!(
        snapshot.jobs,
        vec![done.clone()],
        "the snapshot has recently finished Jobs"
    );

    let job: Job = client.json(&format!("/jobs/{}", job.id));
    let audit_id = job.audit.unwrap();
    let summary: AuditSummary = client.json(&format!("/audits/{audit_id}"));
    assert_eq!(summary.findings[&Finding::Matched], 2);
    let history: Vec<AuditSummary> = client.json("/source-sets/Test%20-%20System/history");
    assert_eq!(history, vec![summary]);

    let report: Report = client.json(&format!("/audits/{audit_id}/report"));
    assert_eq!(report.schema, title_select_api::SCHEMA);
    assert_eq!(report.entries.len(), 3);

    let first: Page<EntryRow> = client.json(&format!("/audits/{audit_id}/entries?limit=2"));
    assert_eq!(first.items.len(), 2);
    let cursor = first.next_cursor.unwrap();
    let rest: Page<EntryRow> = client.json(&format!(
        "/audits/{audit_id}/entries?limit=2&cursor={cursor}"
    ));
    assert_eq!(rest.items.len(), 1);
    assert!(rest.next_cursor.is_none());
    let paged: Vec<_> = first
        .items
        .into_iter()
        .chain(rest.items)
        .map(|r| r.entry)
        .collect();
    assert_eq!(paged, report.entries);
    let unmatched: Page<EntryRow> =
        client.json(&format!("/audits/{audit_id}/entries?finding=unmatched"));
    assert_eq!(unmatched.items.len(), 1);
    let missing: Page<GameRow> = client.json(&format!("/audits/{audit_id}/games?status=missing"));
    assert_eq!(missing.items.len(), 1);
    assert_eq!(missing.items[0].game.name, "Breakout (USA)");

    assert_eq!(status_of(client.get("/audits/999").call()), 404);
    assert_eq!(
        status_of(client.get("/audits/1/entries?cursor=x").call()),
        400
    );
    assert_eq!(status_of(client.get("/nowhere").call()), 404);
}

#[test]
fn the_bulk_ndjson_endpoint_returns_every_entry() {
    let env = Env::new();
    let set = env.tmp.path().join("roots/sets").join(SET);
    let extra: Vec<String> = (0..250).map(|i| format!("extra {i:03}.bin")).collect();
    for name in &extra {
        write_file(&set.join(name), name.as_bytes());
    }
    let service = env.start();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let job: Job = client.start_audit().body_mut().read_json().unwrap();
    let (_, done) = client.follow(job.id);
    let audit_id = done.audit.unwrap();

    let response = client
        .get(&format!("/audits/{audit_id}/entries.ndjson"))
        .call()
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["Content-Type"], "application/x-ndjson");
    let text = response.into_body().read_to_string().unwrap();
    let rows: Vec<EntryRow> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let report: Report = client.json(&format!("/audits/{audit_id}/report"));
    assert_eq!(rows.len(), 253);
    assert_eq!(
        rows.into_iter().map(|r| r.entry).collect::<Vec<_>>(),
        report.entries
    );
}

#[test]
fn a_second_audit_of_the_same_source_set_is_refused_naming_the_active_job() {
    let env = Env::new();
    let service = title_select_server::spawn(Settings {
        hold_jobs: true,
        ..env.settings("")
    })
    .unwrap();
    let client = Client::new(&service);
    client.upload("Test - System.dat", &env.dat());
    client.add_set();
    let first: Job = client.start_audit().body_mut().read_json().unwrap();
    assert_eq!(first.state, JobState::Queued);

    let mut second = client.start_audit();

    assert_eq!(second.status(), 409);
    let body: ErrorBody = second.body_mut().read_json().unwrap();
    assert_eq!(body.job, Some(first.id));
    assert!(
        body.error.contains(&format!("job {}", first.id)),
        "{}",
        body.error
    );
}

#[test]
fn starting_the_service_interrupts_a_job_left_running_and_runs_it_again() {
    let env = Env::new();
    {
        let store = title_select_store::Store::open(&env.store()).unwrap();
        let dat = env.tmp.path().join("in/Test - System.dat");
        std::fs::write(&dat, env.dat()).unwrap();
        store.import_dat(&dat, "No-Intro", None).unwrap();
        store
            .add_source_set(&env.config(""), "sets", SET, "No-Intro/Test - System")
            .unwrap();
        let workers = title_select_store::Workers {
            readers: 1,
            hashers: 1,
        };
        store.enqueue_audit(SET, None, "Operator", workers).unwrap();
        store.claim_job().unwrap();
    }

    let service =
        title_select_server::spawn(env.settings("[jobs]\npace = \"unthrottled\"\n")).unwrap();
    let client = Client::new(&service);

    let jobs: Vec<Job> = client.json("/jobs");
    let (again, first) = (&jobs[0], &jobs[1]);
    assert_eq!(first.state, JobState::Interrupted);
    assert_eq!((again.attempt, again.requeued_from), (2, Some(first.id)));
    let cpus = std::thread::available_parallelism().unwrap().get();
    assert_eq!(again.workers.readers, cpus);
    let (_, done) = client.follow(again.id);
    assert_eq!(done.state, JobState::Succeeded);
}

#[test]
fn the_service_holds_the_store_lock() {
    let env = Env::new();
    let service = env.start();

    let refused = title_select_store::Store::open(&env.store());

    assert!(refused.is_err());
    drop(service);
    title_select_store::Store::open(&env.store()).unwrap();
}
