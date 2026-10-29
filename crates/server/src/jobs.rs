//! Runs queued Jobs on blocking threads, at most `[jobs] concurrent` at once, and broadcasts their progress and states.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use title_select_api::{Job, JobEvent, JobProgress};
use title_select_engine::Progress;
use title_select_store::Store;
use title_select_store::config::Config;
use tokio::sync::{Notify, Semaphore, broadcast};

/// The least time between two progress events of one Job.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
/// How long the runner waits after the store fails to hand it a Job.
const RETRY: Duration = Duration::from_secs(1);

/// Wakes the runner and carries every Job's events to subscribers.
pub struct Jobs {
    wake: Notify,
    events: broadcast::Sender<JobEvent>,
}

impl Jobs {
    pub fn new() -> Jobs {
        Jobs {
            wake: Notify::new(),
            events: broadcast::channel(1024).0,
        }
    }

    /// Announces a Job's new state, and wakes the runner in case it was queued.
    pub fn changed(&self, job: Job) {
        let queued = job.state == title_select_api::JobState::Queued;
        self.publish(JobEvent::State(Box::new(job)));
        if queued {
            self.wake.notify_one();
        }
    }

    pub fn publish(&self, event: JobEvent) {
        let _ = self.events.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<JobEvent> {
        self.events.subscribe()
    }
}

/// Claims and runs Queued Jobs forever.
pub async fn run(store: Arc<Store>, config: Arc<Config>, jobs: Arc<Jobs>) {
    let slots = Arc::new(Semaphore::new(config.jobs.concurrent));
    loop {
        let slot = slots.clone().acquire_owned().await.expect("never closed");
        let claimed = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.claim_job()).await
        };
        let job = match claimed {
            Ok(Ok(Some(job))) => job,
            Ok(Ok(None)) => {
                drop(slot);
                jobs.wake.notified().await;
                continue;
            }
            Ok(Err(err)) => {
                eprintln!("title-select: cannot claim a Job: {err}");
                drop(slot);
                tokio::time::sleep(RETRY).await;
                continue;
            }
            Err(err) => {
                eprintln!("title-select: cannot claim a Job: {err}");
                drop(slot);
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        eprintln!(
            "title-select: Job {} started: Audit of {}",
            job.id, job.source_set
        );
        jobs.changed(job.clone());
        let (store, config, jobs) = (store.clone(), config.clone(), jobs.clone());
        tokio::spawn(async move {
            let (id, version) = (job.id, job.version);
            let ended = {
                let (store, jobs) = (store.clone(), jobs.clone());
                tokio::task::spawn_blocking(move || run_one(&store, &config, &jobs, id, version))
                    .await
            };
            let job = match ended {
                Ok(job) => job,
                Err(err) => {
                    let store = store.clone();
                    let message = format!("the Audit stopped unexpectedly: {err}");
                    tokio::task::spawn_blocking(move || store.fail_job(id, &message).ok())
                        .await
                        .ok()
                        .flatten()
                }
            };
            if let Some(job) = job {
                eprintln!("title-select: Job {} {}", job.id, job.state.as_str());
                jobs.changed(job);
            }
            drop(slot);
            jobs.wake.notify_one();
        });
    }
}

/// Runs one claimed Job's Audit, returning the Job as it ended.
fn run_one(store: &Store, config: &Config, jobs: &Jobs, id: i64, version: u64) -> Option<Job> {
    let last = Mutex::new(None::<Instant>);
    let progress = |p: Progress| {
        let mut last = last.lock().unwrap();
        let done = p.files_done == p.files_total;
        if done || last.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL) {
            *last = Some(Instant::now());
            jobs.publish(JobEvent::Progress(JobProgress {
                job: id,
                version,
                files_done: p.files_done,
                files_total: p.files_total,
            }));
        }
    };
    match store.run_audit_job(config, id, Some(&progress)) {
        Ok(job) => Some(job),
        Err(err) => {
            eprintln!("title-select: Job {id}: {err}");
            match store.job(id) {
                Ok(job) if job.state.is_final() => Some(job),
                _ => store.fail_job(id, &err.to_string()).ok(),
            }
        }
    }
}
