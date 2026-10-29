//! The `/api/v1` routes.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::sse::{Event, KeepAlive, KeepAliveStream, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router, middleware};
use serde::Deserialize;
use title_select_api::{
    AddSourceSet, Dat, ErrorBody, Finding, GameStatus, Health, ImportedDatRevision, JobEvent, Page,
    Root, StartAudit,
};
use title_select_store::config::Config;
use title_select_store::{Error, Store, Workers};
use tokio::sync::{broadcast, mpsc};
use tokio_stream::wrappers::ReceiverStream;

use crate::auth::{Actor, Auth, require_operator};
use crate::jobs::Jobs;

/// The largest DAT file an upload may carry.
pub const UPLOAD_LIMIT: usize = 512 * 1024 * 1024;
const DEFAULT_PAGE: usize = 100;
const MAX_PAGE: usize = 1000;
/// How many bytes of NDJSON are gathered before they are sent.
const NDJSON_CHUNK: usize = 64 * 1024;

#[derive(Clone)]
pub struct AppState {
    pub store: Arc<Store>,
    pub config: Arc<Config>,
    pub jobs: Arc<Jobs>,
    pub workers: Workers,
}

pub fn router(state: AppState, auth: Arc<Auth>) -> Router {
    let operator = Router::new()
        .route("/roots", get(roots))
        .route("/dats", get(dats))
        .route(
            "/dat-revisions",
            get(dat_revisions)
                .post(upload_dat)
                .layer(DefaultBodyLimit::max(UPLOAD_LIMIT)),
        )
        .route("/source-sets", get(source_sets).post(add_source_set))
        .route("/source-sets/{name}", get(source_set))
        .route("/source-sets/{name}/history", get(history))
        .route("/audits", post(start_audit))
        .route("/audits/{id}", get(audit))
        .route("/audits/{id}/report", get(report))
        .route("/audits/{id}/entries", get(entries))
        .route("/audits/{id}/entries.ndjson", get(entries_ndjson))
        .route("/audits/{id}/games", get(games))
        .route("/jobs", get(jobs))
        .route("/jobs/{id}", get(job))
        .route("/jobs/{id}/events", get(job_events))
        .route("/events", get(all_events))
        .layer(middleware::from_fn_with_state(auth, require_operator));
    let api = Router::new()
        .route("/health", get(health))
        .merge(operator)
        .fallback(not_found);
    Router::new()
        .nest(title_select_api::PREFIX, api)
        .fallback(not_found)
        .with_state(state)
}

pub fn error_response(status: StatusCode, error: String, job: Option<i64>) -> Response {
    (status, Json(ErrorBody { error, job })).into_response()
}

/// An error response with an [`ErrorBody`].
pub struct ApiError(StatusCode, String, Option<i64>);

impl ApiError {
    fn bad_request(message: impl Into<String>) -> ApiError {
        ApiError(StatusCode::BAD_REQUEST, message.into(), None)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        error_response(self.0, self.1, self.2)
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> ApiError {
        let status = match &err {
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Refused(_) => StatusCode::BAD_REQUEST,
            Error::JobActive { job, .. } => {
                return ApiError(StatusCode::CONFLICT, err.to_string(), Some(*job));
            }
            Error::Audit(title_select_engine::Error::Dat(_)) => StatusCode::BAD_REQUEST,
            Error::Sqlite(_) | Error::Io(_) | Error::Audit(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError(status, err.to_string(), None)
    }
}

macro_rules! rejection {
    ($($t:ty),*) => {$(
        impl From<$t> for ApiError {
            fn from(err: $t) -> ApiError {
                ApiError(err.status(), err.body_text(), None)
            }
        }
    )*};
}
rejection!(
    JsonRejection,
    PathRejection,
    QueryRejection,
    axum::extract::multipart::MultipartRejection,
    axum::extract::multipart::MultipartError
);

type ApiResult<T> = Result<T, ApiError>;

/// Runs a store call on a blocking thread.
async fn blocking<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Store) -> Result<T, Error> + Send + 'static,
) -> ApiResult<T> {
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || f(&store))
        .await
        .map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string(), None))?
        .map_err(ApiError::from)
}

async fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "no such API path".into(), None)
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".into(),
    })
}

async fn roots(State(state): State<AppState>) -> Json<Vec<Root>> {
    Json(
        state
            .config
            .roots
            .iter()
            .map(|(name, root)| Root {
                name: name.clone(),
                path: root.path.display().to_string(),
                writable: root.writable,
            })
            .collect(),
    )
}

async fn dats(State(state): State<AppState>) -> ApiResult<Json<Vec<Dat>>> {
    let revisions = blocking(&state, Store::dat_revisions).await?;
    let mut dats: BTreeMap<String, Dat> = BTreeMap::new();
    for revision in revisions {
        dats.entry(revision.dat())
            .or_insert_with(|| Dat {
                dat: revision.dat(),
                provider: revision.provider.clone(),
                name: revision.name.clone(),
                revisions: Vec::new(),
            })
            .revisions
            .push(revision);
    }
    Ok(Json(dats.into_values().collect()))
}

async fn dat_revisions(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<title_select_api::DatRevisionRecord>>> {
    Ok(Json(blocking(&state, Store::dat_revisions).await?))
}

/// A multipart form: `provider`, an optional `source_url`, and the DAT as `file`, with its original filename.
async fn upload_dat(
    State(state): State<AppState>,
    multipart: Result<Multipart, axum::extract::multipart::MultipartRejection>,
) -> ApiResult<(StatusCode, Json<ImportedDatRevision>)> {
    let mut multipart = multipart?;
    let (mut provider, mut source_url, mut file) = (None, None, None);
    while let Some(field) = multipart.next_field().await? {
        match field.name() {
            Some("provider") => provider = Some(field.text().await?),
            Some("source_url") => source_url = Some(field.text().await?).filter(|s| !s.is_empty()),
            Some("file") => {
                let name = field.file_name().unwrap_or_default().to_owned();
                file = Some((name, field.bytes().await?));
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "unexpected form field {:?}",
                    other.unwrap_or_default()
                )));
            }
        }
    }
    let provider = provider.ok_or_else(|| ApiError::bad_request("the form has no provider"))?;
    let (name, bytes) = file.ok_or_else(|| ApiError::bad_request("the form has no file"))?;
    if name.is_empty() {
        return Err(ApiError::bad_request("the file has no filename"));
    }
    let (dat_revision, outcome) = blocking(&state, move |store| {
        store.import_dat_bytes(&bytes, &name, &provider, source_url.as_deref())
    })
    .await?;
    let status = match outcome {
        title_select_api::ImportOutcome::Added => StatusCode::CREATED,
        _ => StatusCode::OK,
    };
    Ok((
        status,
        Json(ImportedDatRevision {
            outcome,
            dat_revision,
        }),
    ))
}

async fn source_sets(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<title_select_api::SourceSetRecord>>> {
    Ok(Json(blocking(&state, Store::source_sets).await?))
}

async fn add_source_set(
    State(state): State<AppState>,
    body: Result<Json<AddSourceSet>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::SourceSetRecord>)> {
    let Json(add) = body?;
    let config = state.config.clone();
    let record = blocking(&state, move |store| {
        store.add_source_set(&config, &add.root, &add.path, &add.dat)
    })
    .await?;
    Ok((StatusCode::CREATED, Json(record)))
}

async fn source_set(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<title_select_api::SourceSetRecord>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.source_set(&name)).await?,
    ))
}

async fn history(
    State(state): State<AppState>,
    name: Result<Path<String>, PathRejection>,
) -> ApiResult<Json<Vec<title_select_api::AuditSummary>>> {
    let Path(name) = name?;
    Ok(Json(
        blocking(&state, move |store| store.history(&name)).await?,
    ))
}

/// Queues an Audit Job. It runs whether or not the client stays connected.
async fn start_audit(
    State(state): State<AppState>,
    Extension(Actor(actor)): Extension<Actor>,
    body: Result<Json<StartAudit>, JsonRejection>,
) -> ApiResult<(StatusCode, Json<title_select_api::Job>)> {
    let Json(start) = body?;
    let workers = state.workers;
    let job = blocking(&state, move |store| {
        store.enqueue_audit(
            &start.source_set,
            start.revision.as_deref(),
            &actor,
            workers,
        )
    })
    .await?;
    state.jobs.changed(job.clone());
    Ok((StatusCode::ACCEPTED, Json(job)))
}

async fn audit(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::AuditSummary>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.audit(id)).await?))
}

/// The full `schema: 2` report.
async fn report(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::Report>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.report(id)).await?))
}

#[derive(Deserialize)]
struct EntriesQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    finding: Option<Finding>,
}

#[derive(Deserialize)]
struct GamesQuery {
    cursor: Option<String>,
    limit: Option<usize>,
    status: Option<GameStatus>,
}

/// The cursor is the `seq` of the last item on the previous page.
fn paging(cursor: Option<&str>, limit: Option<usize>) -> ApiResult<(Option<u64>, usize)> {
    let after = cursor
        .map(|c| {
            c.parse()
                .map_err(|_| ApiError::bad_request(format!("invalid cursor {c:?}")))
        })
        .transpose()?;
    let limit = limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_PAGE}"
        )));
    }
    Ok((after, limit))
}

fn to_page<T>((items, next): (Vec<T>, Option<u64>)) -> Json<Page<T>> {
    Json(Page {
        items,
        next_cursor: next.map(|n| n.to_string()),
    })
}

async fn entries(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<EntriesQuery>, QueryRejection>,
) -> ApiResult<Json<Page<title_select_api::EntryRow>>> {
    let (Path(id), Query(query)) = (id?, query?);
    let (after, limit) = paging(query.cursor.as_deref(), query.limit)?;
    let page = blocking(&state, move |store| {
        store.entries(id, after, limit, query.finding)
    })
    .await?;
    Ok(to_page(page))
}

async fn games(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
    query: Result<Query<GamesQuery>, QueryRejection>,
) -> ApiResult<Json<Page<title_select_api::GameRow>>> {
    let (Path(id), Query(query)) = (id?, query?);
    let (after, limit) = paging(query.cursor.as_deref(), query.limit)?;
    let page = blocking(&state, move |store| {
        store.games(id, after, limit, query.status)
    })
    .await?;
    Ok(to_page(page))
}

/// Every Entry of a full Audit, one JSON [`title_select_api::EntryRow`] per line, read in one transaction.
async fn entries_ndjson(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Response> {
    let Path(id) = id?;
    let summary = blocking(&state, move |store| store.audit(id)).await?;
    if summary.pruned {
        return Err(
            Error::Refused(format!("Audit {id} was pruned; only its summary is kept")).into(),
        );
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let store = state.store.clone();
    tokio::task::spawn_blocking(move || {
        let mut chunk = Vec::with_capacity(NDJSON_CHUNK);
        let sent = store.for_each_entry(id, |entry| {
            serde_json::to_writer(&mut chunk, &entry).expect("an Entry serializes");
            chunk.push(b'\n');
            if chunk.len() < NDJSON_CHUNK {
                return true;
            }
            let full = std::mem::replace(&mut chunk, Vec::with_capacity(NDJSON_CHUNK));
            tx.blocking_send(Ok(full.into())).is_ok()
        });
        let last = match sent {
            Ok(()) => Ok(chunk.into()),
            Err(err) => Err(std::io::Error::other(err.to_string())),
        };
        let _ = tx.blocking_send(last);
    });
    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(ReceiverStream::new(rx)),
    )
        .into_response())
}

async fn jobs(State(state): State<AppState>) -> ApiResult<Json<Vec<title_select_api::Job>>> {
    Ok(Json(blocking(&state, |store| store.jobs(100)).await?))
}

async fn job(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<Json<title_select_api::Job>> {
    let Path(id) = id?;
    Ok(Json(blocking(&state, move |store| store.job(id)).await?))
}

type EventStream = Sse<KeepAliveStream<ReceiverStream<Result<Event, Infallible>>>>;

fn sse_event(event: &JobEvent) -> Result<Event, Infallible> {
    Ok(Event::default()
        .event(event.name())
        .json_data(event)
        .expect("a Job event serializes"))
}

/// The Job's `state` now and as it changes, and its `progress` while Running, ending after a final state.
async fn job_events(
    State(state): State<AppState>,
    id: Result<Path<i64>, PathRejection>,
) -> ApiResult<EventStream> {
    let Path(id) = id?;
    // Subscribed before reading the Job, so no change between the two is missed.
    let mut events = state.jobs.subscribe();
    let job = blocking(&state, move |store| store.job(id)).await?;
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        let mut finished = job.state.is_final();
        if tx
            .send(sse_event(&JobEvent::State(Box::new(job))))
            .await
            .is_err()
        {
            return;
        }
        while !finished {
            let received = tokio::select! {
                received = events.recv() => received,
                () = tx.closed() => return,
            };
            let event = match received {
                Ok(event) if event.job_id() == id => event,
                Ok(_) => continue,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let store = state.store.clone();
                    match tokio::task::spawn_blocking(move || store.job(id)).await {
                        Ok(Ok(job)) => JobEvent::State(Box::new(job)),
                        _ => return,
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            };
            finished = matches!(&event, JobEvent::State(job) if job.state.is_final());
            if tx.send(sse_event(&event)).await.is_err() {
                return;
            }
        }
    });
    Ok(Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default()))
}

/// Every Job's `state` changes and `progress`, until the client leaves.
async fn all_events(State(state): State<AppState>) -> EventStream {
    let mut events = state.jobs.subscribe();
    let (tx, rx) = mpsc::channel(16);
    tokio::spawn(async move {
        loop {
            let received = tokio::select! {
                received = events.recv() => received,
                () = tx.closed() => return,
            };
            let event = match received {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return,
            };
            if tx.send(sse_event(&event)).await.is_err() {
                return;
            }
        }
    });
    Sse::new(ReceiverStream::new(rx)).keep_alive(KeepAlive::default())
}
