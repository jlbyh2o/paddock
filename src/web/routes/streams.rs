//! The three append-only collections, fetched by sequence rather than carried in the
//! snapshot: the engine log, a job's output, and the request ring.

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::actions;
use crate::ft::proc::LogLine;
use crate::ft::types::RequestRecord;
use crate::web::state::{ApiError, ApiResult, Body, Shared, Q};

/// The envelope every sequenced collection shares. `first_seq` against the sequence a
/// client last rendered is what tells it whether anything was lost in between.
#[derive(Serialize)]
pub struct Page<T> {
    items: Vec<T>,
    first_seq: u64,
    last_seq: u64,
    dropped: u64,
    next_after: u64,
}

#[derive(Deserialize)]
pub struct SeqQuery {
    #[serde(default)]
    after: u64,
    limit: Option<usize>,
}

// ---------------------------------------------------------------- logs

pub async fn logs(State(state): State<Shared>, Q(q): Q<SeqQuery>) -> Json<Page<LogLine>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    state.read(|app| {
        let stats = app.engine.log.stats();
        let items = app.engine.log.since(q.after, limit);
        let next = items.last().map(|l| l.seq).unwrap_or(q.after.max(stats.last_seq));
        Json(Page {
            items,
            first_seq: stats.first_seq,
            last_seq: stats.last_seq,
            dropped: stats.dropped,
            next_after: next,
        })
    })
}

#[derive(Deserialize)]
pub struct Empty {}

pub async fn clear_logs(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    state.write(actions::clear_logs);
    Json(serde_json::json!({ "status": "ok" }))
}

// ---------------------------------------------------------------- requests

/// A record plus the sequence it was appended at, and the decode rate the detail pane
/// shows — which the engine does not send and the browser must not have to work out.
#[derive(Serialize)]
pub struct RequestOut<'a> {
    seq: u64,
    #[serde(flatten)]
    record: &'a RequestRecord,
    decode_tps: Option<f64>,
}

pub async fn requests(State(state): State<Shared>, Q(q): Q<SeqQuery>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let limit = q.limit.unwrap_or(200).clamp(1, 512);
    let body = state.read(|app| {
        let v = &app.requests_view;
        let items: Vec<RequestOut<'_>> = v
            .since(q.after, limit)
            .into_iter()
            .map(|(seq, r)| RequestOut { seq, record: r, decode_tps: decode_tps(r) })
            .collect();
        let next = items.last().map(|i| i.seq).unwrap_or(q.after.max(v.last_seq()));
        serde_json::to_string(&Page {
            items,
            first_seq: v.first_seq(),
            last_seq: v.last_seq(),
            dropped: v.dropped(),
            next_after: next,
        })
    });
    match body {
        Ok(json) => {
            ([(axum::http::header::CONTENT_TYPE, "application/json")], json).into_response()
        }
        Err(e) => ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not serialize the request ring: {e}"),
        )
        .into_response(),
    }
}

/// Completion tokens over the wall clock. `None` unless both are real numbers, because a
/// zero here reads as "the engine is broken" rather than "nothing was measured".
fn decode_tps(r: &RequestRecord) -> Option<f64> {
    let completion = r.completion_tokens?;
    (r.duration_ms > 0 && completion > 0)
        .then(|| completion as f64 / (r.duration_ms as f64 / 1000.0))
}

#[derive(Deserialize)]
pub struct PauseRequest {
    paused: bool,
}

pub async fn pause_requests(
    State(state): State<Shared>,
    Body(req): Body<PauseRequest>,
) -> Json<serde_json::Value> {
    state.write(|app| actions::set_requests_paused(app, req.paused));
    Json(serde_json::json!({ "paused": req.paused }))
}

pub async fn clear_requests(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    state.write(actions::clear_requests);
    Json(serde_json::json!({ "status": "ok" }))
}

// ---------------------------------------------------------------- job output

#[derive(Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    offset: u64,
    limit: Option<u64>,
}

#[derive(Serialize)]
pub struct JobOutput {
    id: u64,
    offset: u64,
    next_offset: u64,
    eof: bool,
    truncated: bool,
    lines: Vec<String>,
}

/// The tail of one job's log **file**.
///
/// The file rather than the in-memory ring, because a byte offset is the only stable
/// cursor into an append-only log and the file survives a daemon restart. The progress
/// protocol is filtered back out so the browser sees what the TUI's output pane shows:
/// `spawn_job_reader` keeps those lines out of the ring but writes them to the file.
pub async fn job_output(
    State(state): State<Shared>,
    Path(id): Path<u64>,
    Q(q): Q<OutputQuery>,
) -> ApiResult<Json<JobOutput>> {
    let limit = q.limit.unwrap_or(64 * 1024).clamp(1, 1 << 20);
    let path = state
        .read(|app| app.jobs.iter().find(|j| j.id == id).map(|j| j.log_path.clone()))
        .ok_or_else(|| ApiError::not_found(format!("no job with id {id}")))?;

    let read = tokio::task::spawn_blocking(move || read_tail(&path, q.offset, limit))
        .await
        .map_err(|e| {
            ApiError::new(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not read the job log: {e}"),
            )
        })?;

    let Tail { offset, next_offset, eof, truncated, text } = read.map_err(|e| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("could not read the job log: {e}"),
        )
    })?;

    Ok(Json(JobOutput {
        id,
        offset,
        next_offset,
        eof,
        truncated,
        lines: text.lines().filter(|l| !is_progress(l)).map(str::to_string).collect(),
    }))
}

/// The machine-readable progress protocol, which the output pane has never shown.
fn is_progress(line: &str) -> bool {
    line.starts_with("FTCONVERT ")
        || line.starts_with("FTBENCH ")
        || line.starts_with("FTBENCH_OUT ")
}

struct Tail {
    offset: u64,
    next_offset: u64,
    eof: bool,
    truncated: bool,
    text: String,
}

fn read_tail(path: &std::path::Path, offset: u64, limit: u64) -> std::io::Result<Tail> {
    use std::io::{Read, Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    // A rotated or truncated file leaves a held offset pointing past the end; restarting
    // at zero and saying so beats returning nothing forever.
    let truncated = offset > size;
    let start = if truncated { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;

    let want = limit.min(size.saturating_sub(start)) as usize;
    let mut buf = vec![0u8; want];
    let read = file.read(&mut buf)?;
    buf.truncate(read);

    // Never hand back half a line: the next request continues from a line boundary, so
    // the tail of a file being written to arrives whole on the following poll.
    let end = match buf.iter().rposition(|b| *b == b'\n') {
        Some(i) => i + 1,
        // Nothing but a partial line, unless this is the whole rest of the file.
        None if start + read as u64 >= size => read,
        None => 0,
    };
    let text = String::from_utf8_lossy(&buf[..end]).into_owned();
    let next_offset = start + end as u64;
    Ok(Tail { offset: start, next_offset, eof: next_offset >= size, truncated, text })
}
