//! The headless client: starts an Audit Job on a running Service and follows it to completion.

use std::io::BufRead;
use std::path::Path;

use title_select_api::{
    ErrorBody, Finding, GameStatus, Job, JobEvent, JobProgress, JobState, PREFIX, Report,
    StartAudit,
};

pub struct Request<'a> {
    pub server: &'a str,
    pub set: String,
    pub revision: Option<String>,
    pub token_file: Option<&'a Path>,
}

struct Client {
    agent: ureq::Agent,
    base: String,
    authorization: String,
}

impl Client {
    fn url(&self, path: &str) -> String {
        format!("{}{PREFIX}{path}", self.base)
    }

    fn unreachable(&self, err: ureq::Error) -> String {
        format!("cannot reach the Service at {}: {err}", self.base)
    }

    /// The body of a successful response, else the error the Service gave.
    fn check(
        &self,
        mut response: ureq::http::Response<ureq::Body>,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let status = response.status();
        if status.is_success() {
            return Ok(response);
        }
        let message = response
            .body_mut()
            .read_json::<ErrorBody>()
            .map(|b| b.error)
            .unwrap_or_else(|_| status.to_string());
        if status == ureq::http::StatusCode::UNAUTHORIZED {
            return Err(format!("the Service refused the Operator token: {message}"));
        }
        Err(message)
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let response = self
            .agent
            .get(self.url(path))
            .header("Authorization", &self.authorization)
            .call()
            .map_err(|e| self.unreachable(e))?;
        self.check(response)?
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_json()
            .map_err(|e| format!("{path}: {e}"))
    }

    /// Follows the Job's events until a final state, reconnecting if the stream ends early.
    fn follow(&self, mut job: Job) -> Result<Job, String> {
        let mut shown = None;
        while !job.state.is_final() {
            let response = self
                .agent
                .get(self.url(&format!("/jobs/{}/events", job.id)))
                .header("Authorization", &self.authorization)
                .header("Accept", "text/event-stream")
                .call()
                .map_err(|e| self.unreachable(e))?;
            let reader = std::io::BufReader::new(self.check(response)?.into_body().into_reader());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                match serde_json::from_str::<JobEvent>(data.trim_start()) {
                    Ok(JobEvent::State(next)) => {
                        if next.state != job.state {
                            eprintln!("Job {}: {}", next.id, next.state.as_str());
                        }
                        job = *next;
                    }
                    Ok(JobEvent::Progress(progress)) => show_progress(progress, &mut shown),
                    Err(err) => return Err(format!("unexpected event from the Service: {err}")),
                }
            }
            if !job.state.is_final() {
                job = self.get(&format!("/jobs/{}", job.id))?;
            }
        }
        Ok(job)
    }
}

/// Shows progress every tenth of the way.
fn show_progress(progress: JobProgress, shown: &mut Option<usize>) {
    let tenth = (progress.files_done * 10)
        .checked_div(progress.files_total)
        .unwrap_or(10);
    if *shown != Some(tenth) {
        *shown = Some(tenth);
        eprintln!(
            "Job {}: {}/{} files read",
            progress.job, progress.files_done, progress.files_total
        );
    }
}

fn read_token(path: Option<&Path>) -> Result<String, String> {
    let path = path.ok_or(
        "no Operator token: pass --token-file <file> or set TITLE_SELECT_OPERATOR_TOKEN_FILE",
    )?;
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let token = text.trim();
    if token.is_empty() {
        return Err(format!(
            "{}: the Operator token file is empty",
            path.display()
        ));
    }
    Ok(token.to_owned())
}

/// Starts the Audit Job, follows it to completion and returns its report, showing progress and a summary on stderr.
pub fn audit(request: Request) -> Result<Report, String> {
    let token = read_token(request.token_file)?;
    let client = Client {
        agent: ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .into(),
        base: request.server.trim_end_matches('/').to_owned(),
        authorization: format!("Bearer {token}"),
    };
    let start = StartAudit {
        source_set: request.set,
        revision: request.revision,
    };
    let response = client
        .agent
        .post(client.url("/audits"))
        .header("Authorization", &client.authorization)
        .send_json(&start)
        .map_err(|e| client.unreachable(e))?;
    let job: Job = client
        .check(response)?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())?;
    eprintln!(
        "Job {}: {}: Audit of {} against DAT Revision {}",
        job.id,
        job.state.as_str(),
        job.source_set,
        job.dat_revision
    );
    let job = client.follow(job)?;
    if job.state != JobState::Succeeded {
        return Err(format!(
            "Job {} {}: {}",
            job.id,
            job.state.as_str(),
            job.error.as_deref().unwrap_or("no reason given")
        ));
    }
    let audit = job.audit.ok_or("the Job succeeded without an Audit")?;
    let report: Report = client.get(&format!("/audits/{audit}/report"))?;
    let findings = Finding::ALL.iter().map(|f| {
        format!(
            "{}={}",
            f.as_str(),
            report.summary.findings.get(f).unwrap_or(&0)
        )
    });
    let games = GameStatus::ALL.iter().map(|g| {
        format!(
            "{}={}",
            g.as_str(),
            report.summary.games.get(g).unwrap_or(&0)
        )
    });
    let counts: Vec<String> = findings.chain(games).collect();
    eprintln!(
        "Audit {audit} of {} against {} ({}): {}",
        job.source_set,
        report.dat_revision.sha256,
        report.dat_revision.version,
        counts.join(" ")
    );
    Ok(report)
}
