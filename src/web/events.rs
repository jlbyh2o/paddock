//! `GET /api/events` — the snapshot stream.
//!
//! State is absolute, not incremental: every frame carries a whole `Snapshot`, so a
//! reconnecting browser needs no replay and `Last-Event-ID` is ignored. Frames are
//! coalesced to ten a second, which is faster than anyone reads and slower than a burst
//! of telemetry can produce.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;

use super::snapshot;
use super::state::Shared;

/// At most ten snapshots a second, however fast the state underneath changes.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// A heartbeat once a second, so a proxy between here and the browser does not decide an
/// idle connection is a dead one.
const HEARTBEAT: Duration = Duration::from_secs(1);

pub async fn events(
    State(state): State<Shared>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = frames(state);
    Sse::new(stream).keep_alive(KeepAlive::default().interval(Duration::from_secs(30)))
}

/// The frame generator. Written as a channel plus a task rather than a generator macro,
/// which keeps the dependency list shorter than the code.
fn frames(state: Shared) -> impl Stream<Item = Result<Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(4);
    tokio::spawn(async move {
        let mut changed = state.changed.subscribe();
        changed.mark_unchanged();

        // `retry` first, so a browser that loses the connection comes back in two
        // seconds rather than on whatever default its engine picks.
        if tx.send(Ok(Event::default().comment("ft-man"))).await.is_err() {
            return;
        }
        if tx.send(Ok(Event::default().retry(Duration::from_millis(2000)))).await.is_err() {
            return;
        }
        // And a snapshot immediately: a client that has just connected has nothing.
        if send_snapshot(&state, &tx).await.is_err() {
            return;
        }

        loop {
            let woke = tokio::time::timeout(HEARTBEAT, changed.changed()).await;
            match woke {
                // State changed.
                Ok(Ok(())) => {
                    if send_snapshot(&state, &tx).await.is_err() {
                        return;
                    }
                    // Hold the floor briefly so a burst of messages costs one frame.
                    tokio::time::sleep(MIN_INTERVAL).await;
                    changed.mark_unchanged();
                }
                // The sender is gone, which means the daemon is shutting down.
                Ok(Err(_)) => return,
                // Nothing changed for a second.
                Err(_) => {
                    let beat = Event::default().event("heartbeat").data("{}");
                    if tx.send(Ok(beat)).await.is_err() {
                        return;
                    }
                }
            }
        }
    });
    tokio_stream_wrapper(rx)
}

async fn send_snapshot(
    state: &Shared,
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
) -> Result<(), ()> {
    let seq = state.next_seq();
    // Serialized under the lock and sent after it: `Snapshot` borrows from `App`, so it
    // cannot outlive the guard, and nothing may be awaited while the guard is held.
    let json = state.read(|app| serde_json::to_string(&snapshot::build(app, seq)));
    let Ok(json) = json else {
        tracing::warn!("could not serialize a snapshot");
        return Ok(());
    };
    let event = Event::default().event("snapshot").id(seq.to_string()).data(json);
    tx.send(Ok(event)).await.map_err(|_| ())
}

/// `ReceiverStream` without pulling in `tokio-stream` for one type.
fn tokio_stream_wrapper(
    rx: tokio::sync::mpsc::Receiver<Result<Event, Infallible>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) })
}
