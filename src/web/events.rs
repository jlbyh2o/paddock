//! `GET /api/events` — the snapshot stream.
//!
//! State is absolute, not incremental: every frame carries a whole `Snapshot`, so a
//! reconnecting browser needs no replay and `Last-Event-ID` is ignored. Frames are
//! coalesced to ten a second, which is faster than anyone reads and slower than a burst
//! of telemetry can produce.
//!
//! One task builds them. Serializing the same document once per connected client was the
//! shape this replaced: the broadcaster builds a frame when state changes, publishes the
//! finished `Arc<str>` through a watch channel, and every client task only forwards it.

use std::convert::Infallible;
use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;

use super::state::{Frame, Shared};

/// At most ten snapshots a second, however fast the state underneath changes.
const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// A heartbeat once a second, so a proxy between here and the browser does not decide an
/// idle connection is a dead one.
const HEARTBEAT: Duration = Duration::from_secs(1);

/// The one task that turns state changes into frames.
///
/// Started once, by `web::run`, alongside the ticker and the message drain.
pub fn spawn_broadcaster(state: Shared) {
    tokio::spawn(async move {
        let mut changed = state.changed.subscribe();
        changed.mark_unchanged();
        // A first frame, so a client connecting before anything has happened still has
        // state to render.
        state.publish();
        loop {
            if changed.changed().await.is_err() {
                // The sender is gone, which means the daemon is shutting down.
                return;
            }
            // Mark seen *before* building, then hold the floor. The other order was a
            // silent dropped update: a change landing during the hold was marked as
            // already-seen on the way out, and nothing woke this task again until the
            // one after it.
            changed.mark_unchanged();
            state.publish();
            tokio::time::sleep(MIN_INTERVAL).await;
        }
    });
}

pub async fn events(
    State(state): State<Shared>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = frames(state);
    Sse::new(stream).keep_alive(KeepAlive::default().interval(Duration::from_secs(30)))
}

/// One client's view of the published frames. Written as a channel plus a task rather than
/// a generator macro, which keeps the dependency list shorter than the code.
fn frames(state: Shared) -> impl Stream<Item = Result<Event, Infallible>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(4);
    tokio::spawn(async move {
        let mut frames = state.frames.subscribe();

        // `retry` first, so a browser that loses the connection comes back in two
        // seconds rather than on whatever default its engine picks.
        if tx.send(Ok(Event::default().comment("paddock"))).await.is_err() {
            return;
        }
        if tx.send(Ok(Event::default().retry(Duration::from_millis(2000)))).await.is_err() {
            return;
        }

        // And a snapshot immediately: a client that has just connected has nothing. The
        // newest published frame if there is one, otherwise build the first.
        let held = frames.borrow_and_update().clone();
        let first = match held {
            Some(frame) => Some(frame),
            None => state.publish(),
        };
        if let Some(frame) = first {
            if send(&tx, &frame).await.is_err() {
                return;
            }
        }

        loop {
            match tokio::time::timeout(HEARTBEAT, frames.changed()).await {
                // A new frame was published.
                Ok(Ok(())) => {
                    let frame = frames.borrow_and_update().clone();
                    if let Some(frame) = frame {
                        if send(&tx, &frame).await.is_err() {
                            return;
                        }
                    }
                }
                // The daemon is shutting down.
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

async fn send(
    tx: &tokio::sync::mpsc::Sender<Result<Event, Infallible>>,
    frame: &Frame,
) -> Result<(), ()> {
    let event =
        Event::default().event("snapshot").id(frame.seq.to_string()).data(frame.json.as_ref());
    tx.send(Ok(event)).await.map_err(|_| ())
}

/// `ReceiverStream` without pulling in `tokio-stream` for one type.
fn tokio_stream_wrapper(
    rx: tokio::sync::mpsc::Receiver<Result<Event, Infallible>>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|item| (item, rx)) })
}
