//! The background samplers both front ends run.
//!
//! Neither the terminal nor the browser is what decides how often `/health` is polled or
//! how often NVML is read, so these live outside both. `ft-man` and `ft-man web` start
//! exactly the same three tasks against exactly the same `App`.

use std::time::Duration;

use tokio::sync::mpsc;

use crate::ft;
use crate::probe;
use crate::ui::app::{App, Message, Telemetry};

/// Start every background sampler for `app`.
pub fn spawn_all(app: &App, tx: mpsc::UnboundedSender<Message>) {
    spawn_telemetry(app, tx.clone());
    spawn_hardware(tx.clone());
    spawn_checkout(app, tx.clone());
    spawn_architectures(app, tx);
}

/// Watch the FreeToken checkout this machine builds from.
///
/// The only poll in ft-man that leaves the machine: `git fetch` reaches the remote, so it
/// runs on a timer measured in minutes rather than on the UI tick. The first check is on
/// this task too, not on the startup path — a daemon that came up with the network down
/// would otherwise block its first frame on a fetch that cannot succeed, and a daemon
/// left running for days would never notice a commit pushed after it started.
pub fn spawn_checkout(app: &App, tx: mpsc::UnboundedSender<Message>) {
    let Some(dir) = crate::ft::checkout::locate(&app.config.freetoken, app.ft.as_ref()) else {
        tracing::debug!("no FreeToken checkout found; skipping the upstream check");
        return;
    };
    tracing::info!(path = %dir.display(), "watching the FreeToken checkout");
    let period = Duration::from_secs(app.config.freetoken.checkout_poll_min.saturating_mul(60));
    tokio::spawn(async move {
        loop {
            let dir = dir.clone();
            let read = tokio::task::spawn_blocking(move || crate::ft::checkout::check(&dir)).await;
            let checkout = read.ok().flatten().map(Box::new);
            if tx.send(Message::Checkout(checkout)).is_err() {
                break;
            }
            if period.is_zero() {
                break;
            }
            tokio::time::sleep(period).await;
        }
    });
}

/// Poll the server's control plane on its own cadence.
///
/// `/health` is cheap and always answered, so it drives the loop; `/v1/stats` and
/// `/v1/cache/status` are only meaningful once the engine is serving, and skipping them
/// while it loads keeps a loading engine from being polled pointlessly for minutes.
pub fn spawn_telemetry(app: &App, tx: mpsc::UnboundedSender<Message>) {
    let mut client = app.client.clone();
    let mut endpoint = app.endpoint_tx.subscribe();
    let timeout = Duration::from_millis(app.config.server.timeout_ms);
    let period = Duration::from_millis(app.config.server.poll_ms.max(200));
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut cursor = 0u64;
        loop {
            ticker.tick().await;
            // Follow a port change in the serve configuration rather than polling an
            // address nothing is listening on.
            if endpoint.has_changed().unwrap_or(false) {
                let url = endpoint.borrow_and_update().clone();
                if let Ok(next) = ft::Client::new(&url, timeout) {
                    client = next;
                    cursor = 0;
                }
            }
            let mut t = Telemetry { at: Some(std::time::Instant::now()), ..Default::default() };
            match client.health().await {
                Ok(health) => {
                    let ready = health.is_ready();
                    t.health = Some(health);
                    if ready {
                        t.stats = client.stats().await.ok();
                        t.cache = client.cache_status().await.ok();
                        if let Ok(page) = client.requests(cursor, 200).await {
                            if !page.entries.is_empty() || page.next_cursor != cursor {
                                cursor = page.next_cursor;
                                let _ = tx.send(Message::Requests {
                                    entries: page.entries,
                                    next_cursor: cursor,
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    // A connection refused while nothing is running is the normal state,
                    // not an error worth a toast — the Dashboard shows it as "not
                    // running" and that is enough.
                    t.error = Some(format!("{e:#}"));
                    cursor = 0;
                }
            }
            if tx.send(Message::Telemetry(Box::new(t))).is_err() {
                break;
            }
        }
    });
}

/// Read FreeToken's model registry once, so the Hub can say definitively whether an
/// architecture is supported instead of guessing from a list baked into ft-man.
pub fn spawn_architectures(app: &App, tx: mpsc::UnboundedSender<Message>) {
    let Some(ft) = app.ft.clone() else { return };
    let Some(argv) = ft::preflight::architectures_command(&ft) else { return };
    let env = app.config.freetoken.env.clone();
    tokio::spawn(async move {
        let mut cmd = tokio::process::Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        for (k, v) in &env {
            cmd.env(k, v);
        }
        let result = match cmd.output().await {
            Ok(out) => ft::preflight::parse_architectures(&String::from_utf8_lossy(&out.stdout)),
            Err(e) => Err(e.to_string()),
        };
        let _ = tx.send(Message::Architectures(result));
    });
}

/// Sample GPUs and host memory on a blocking thread — NVML and `/proc` reads are
/// synchronous, and doing them on the runtime would stall other tasks.
pub fn spawn_hardware(tx: mpsc::UnboundedSender<Message>) {
    std::thread::spawn(move || {
        let mut probe = probe::Probe::new();
        loop {
            let gpus = probe.gpus();
            let host = probe.host();
            if tx.send(Message::Hardware { gpus, host }).is_err() {
                break;
            }
            std::thread::sleep(probe::SAMPLE_INTERVAL);
        }
    });
}
