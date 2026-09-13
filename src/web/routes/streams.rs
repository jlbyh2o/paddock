//! The three append-only collections, fetched by sequence rather than carried in the
//! snapshot: the engine log, a job's output, and the request ring.

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::actions;
use crate::ft::proc::LogLine;
use crate::ft::types::RequestRecord;
use crate::ui::views::logs::{classify, Severity};
use crate::web::state::{ApiError, ApiResult, Body, Empty, Shared, P, Q};

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

/// One engine log line, plus how the terminal would color it.
///
/// Classified here rather than in the browser: the rule is `views::logs::classify`, the
/// terminal reads the same function for the same text, and a second implementation would
/// sooner or later color one machine's log two ways.
#[derive(Serialize)]
pub struct LogLineOut {
    #[serde(flatten)]
    line: LogLine,
    severity: Severity,
}

pub async fn logs(State(state): State<Shared>, Q(q): Q<SeqQuery>) -> Json<Page<LogLineOut>> {
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    state.read(|app| {
        let stats = app.engine.log.stats();
        let items: Vec<LogLineOut> = app
            .engine
            .log
            .since(q.after, limit)
            .into_iter()
            .map(|line| LogLineOut { severity: classify(&line.text, line.err), line })
            .collect();
        let next = items.last().map(|l| l.line.seq).unwrap_or(q.after.max(stats.last_seq));
        Json(Page {
            items,
            first_seq: stats.first_seq,
            last_seq: stats.last_seq,
            dropped: stats.dropped,
            next_after: next,
        })
    })
}

pub async fn clear_logs(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    // Clearing an empty ring changed nothing.
    state.write_if(|app| {
        let had = app.engine.log.stats().count > 0;
        actions::clear_logs(app);
        ((), had)
    });
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
    state.write_if(|app| {
        let changed = app.requests_view.paused != req.paused;
        actions::set_requests_paused(app, req.paused);
        ((), changed)
    });
    Json(serde_json::json!({ "paused": req.paused }))
}

pub async fn clear_requests(
    State(state): State<Shared>,
    Body(_): Body<Empty>,
) -> Json<serde_json::Value> {
    state.write_if(|app| {
        let had = !app.requests_view.entries.is_empty();
        actions::clear_requests(app);
        ((), had)
    });
    Json(serde_json::json!({ "status": "ok" }))
}

// ---------------------------------------------------------------- job output

#[derive(Deserialize)]
pub struct OutputQuery {
    #[serde(default)]
    offset: u64,
    limit: Option<u64>,
}

/// One line of a job's output, classified exactly as an engine log line is.
#[derive(Serialize)]
pub struct JobOutputLine {
    text: String,
    severity: Severity,
}

#[derive(Serialize)]
pub struct JobOutput {
    id: u64,
    offset: u64,
    next_offset: u64,
    eof: bool,
    truncated: bool,
    lines: Vec<JobOutputLine>,
}

/// The tail of one job's log **file**.
///
/// The file rather than the in-memory ring, because a byte offset is the only stable
/// cursor into an append-only log and the file survives a daemon restart. The progress
/// protocol is filtered back out so the browser sees what the TUI's output pane shows:
/// `spawn_job_reader` keeps those lines out of the ring but writes them to the file.
pub async fn job_output(
    State(state): State<Shared>,
    // `P`, not axum's `Path`: a non-numeric id rejected by the extractor would otherwise
    // be the one `/api` response that is plain text rather than the error envelope.
    P(id): P<u64>,
    Q(q): Q<OutputQuery>,
) -> ApiResult<Json<JobOutput>> {
    let limit = q.limit.unwrap_or(64 * 1024).clamp(1, 1 << 20);
    let (path, running) = state
        .read(|app| {
            app.jobs.iter().find(|j| j.id == id).map(|j| (j.log_path.clone(), j.is_running()))
        })
        .ok_or_else(|| ApiError::not_found(format!("no job with id {id}")))?;

    let read = tokio::task::spawn_blocking(move || read_tail(&path, q.offset, limit, running))
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
        lines: text
            .lines()
            .filter(|l| !is_progress(l))
            // `err` is unrecoverable from a file whose streams are merged, so every line
            // is classified by content — which is what the terminal does anyway.
            .map(|l| JobOutputLine { text: l.to_string(), severity: classify(l, false) })
            .collect(),
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

/// Read at most `limit` bytes from `offset`, stopping on a line boundary.
///
/// `running` is what decides the partial trailing line. While the job is still writing,
/// half a line is half a sentence and the rest arrives on the next poll, so it is withheld.
/// Once the job has stopped, nothing more is coming: withholding it would hide the last
/// line a crash printed, which is usually the only one worth reading.
fn read_tail(
    path: &std::path::Path,
    offset: u64,
    limit: u64,
    running: bool,
) -> std::io::Result<Tail> {
    use std::io::{Seek, SeekFrom};

    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    // A rotated or truncated file leaves a held offset pointing past the end; restarting
    // at zero and saying so beats returning nothing forever.
    let truncated = offset > size;
    let start = if truncated { 0 } else { offset };
    file.seek(SeekFrom::Start(start))?;

    let mut want = limit.min(size.saturating_sub(start)) as usize;
    // Set once the window had to be widened past `limit` to find a line ending. The
    // widened read exists to complete *one* over-long line, so it stops at the first
    // newline rather than the last: everything past it is an ordinary page's worth.
    let mut grown = false;
    let mut buf;
    let mut end;
    loop {
        file.seek(SeekFrom::Start(start))?;
        buf = vec![0u8; want];
        let read = read_fully(&mut file, &mut buf)?;
        buf.truncate(read);
        let at_end = start + read as u64 >= size;

        end = if at_end && !running {
            // The job has stopped and this is the tail of its file. Nothing more is
            // coming, so withholding a partial last line would hide it forever — and the
            // last line a crash printed is usually the only one worth reading.
            read
        } else if grown {
            buf.iter().position(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0)
        } else {
            buf.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0)
        };
        // A single line longer than `limit` has no newline in the window, so the branch
        // above hands back nothing and `next_offset == offset` — the same request,
        // forever. Widen the read instead: a long line is a long line, not a stall.
        if end > 0 || read == 0 || at_end {
            break;
        }
        want = want.saturating_mul(2).max(8192).min(size.saturating_sub(start) as usize);
        grown = true;
    }

    let text = String::from_utf8_lossy(&buf[..end]).into_owned();
    let next_offset = start + end as u64;
    Ok(Tail { offset: start, next_offset, eof: next_offset >= size, truncated, text })
}

/// Fill `buf` as far as the file allows. One `read` can come back short for reasons that
/// have nothing to do with the end of the file, and a short read here reads as a truncated
/// line.
fn read_fully(file: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::io::Read;
    let mut total = 0;
    while total < buf.len() {
        match file.read(&mut buf[total..])? {
            0 => break,
            n => total += n,
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("ft-man-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// A job still writing has its half-written last line withheld, so the browser never
    /// renders half a sentence and then the same half again with the rest appended.
    #[test]
    fn a_partial_trailing_line_is_withheld_only_while_the_job_runs() {
        let path = tmp("partial.log", "first\nsecond\npart");

        let tail = read_tail(&path, 0, 1 << 20, true).unwrap();
        assert_eq!(tail.text, "first\nsecond\n");
        assert_eq!(tail.next_offset, 13);
        assert!(!tail.eof, "there are bytes left that were deliberately not returned");

        // Once it has stopped, the last line is all there will ever be of it.
        let tail = read_tail(&path, 0, 1 << 20, false).unwrap();
        assert_eq!(tail.text, "first\nsecond\npart");
        assert!(tail.eof);
    }

    /// A line longer than the window used to return nothing at all, with `next_offset`
    /// equal to `offset` — the client then asked for the same bytes forever.
    #[test]
    fn a_line_longer_than_the_limit_is_returned_whole_rather_than_stalling() {
        let long = "x".repeat(5000);
        let path = tmp("long.log", &format!("{long}\nafter\n"));

        let tail = read_tail(&path, 0, 16, true).unwrap();
        assert!(tail.next_offset > 0, "a read that returns nothing new is a stalled client");
        assert_eq!(tail.text, format!("{long}\n"));
        assert_eq!(tail.next_offset, long.len() as u64 + 1);

        // And the next page continues from there.
        let tail = read_tail(&path, tail.next_offset, 16, true).unwrap();
        assert_eq!(tail.text, "after\n");
        assert!(tail.eof);
    }

    #[test]
    fn an_offset_past_the_end_restarts_and_says_so() {
        let path = tmp("short.log", "one\n");
        let tail = read_tail(&path, 9999, 1 << 20, false).unwrap();
        assert!(tail.truncated);
        assert_eq!(tail.offset, 0);
        assert_eq!(tail.text, "one\n");
    }
}
