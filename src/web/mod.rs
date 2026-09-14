//! `ft-man web` — the same `App` the TUI runs, served over HTTP.
//!
//! One `App` behind one mutex, one task draining its message channel, one ticker, and a
//! router whose handlers lock, act and unlock. Nothing here knows anything about what an
//! action does: every route calls the same [`crate::actions`] function a key does.

mod assets;
pub mod auth;
mod events;
mod guard;
mod routes;
pub mod snapshot;
pub mod state;

#[cfg(test)]
mod tests;

use std::time::Duration;

use anyhow::{Context, Result};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::mpsc;

use crate::config::{Config, Profiles};
use crate::ft::Freetoken;
use crate::ui::app::{App, Message};
use state::{Shared, WebState};

/// Serve the web interface until a signal asks us to stop.
pub async fn run(
    config: Config,
    ft: Result<Freetoken, String>,
    listen: String,
    token: Option<String>,
) -> Result<()> {
    let profiles = Profiles::load().unwrap_or_default();
    let (tx, rx) = mpsc::unbounded_channel::<Message>();

    let (ft_ok, ft_err) = match ft {
        Ok(f) => (Some(f), None),
        Err(e) => (None, Some(e)),
    };
    let mut app = App::new(config, profiles, ft_ok, ft_err.clone(), tx.clone())?;
    // Probed once at startup exactly as the TUI does it, and for the same reason: the
    // Dashboard says which FreeToken is answering, and `--version` costs a process
    // spawn, not a poll. Without this the web snapshot carries `ft_version: null`
    // forever and the Engine pane simply omits the line.
    let ft_version = app.ft.as_ref().and_then(crate::ft::locate::probe_version);
    app.set_ft_version(ft_version);
    if let Some(e) = ft_err {
        app.error(e);
    }
    app.request_scan();
    crate::runtime::spawn_all(&app, tx.clone());

    let tick = Duration::from_millis(app.config.ui.tick_ms);
    let auth = auth::Auth::new(token).map_err(|e| anyhow::anyhow!("{e}"))?;
    let authenticated = auth.required();
    let state = WebState::new(app, auth);

    spawn_drain(state.clone(), rx);
    spawn_ticker(state.clone(), tick);
    events::spawn_broadcaster(state.clone());

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    let bound = listener.local_addr().map(|a| a.to_string()).unwrap_or(listen);
    println!(
        "ft-man {} serving the web interface on http://{bound} ({})",
        env!("CARGO_PKG_VERSION"),
        if authenticated { "token required" } else { "no authentication" }
    );
    tracing::info!(address = %bound, auth = authenticated, "web interface listening");

    axum::serve(listener, router(state.clone()))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving the web interface")?;

    // Exactly as the TUI exits: the engine is a long-lived service and the state file
    // lets a later run re-attach, so only a stop that was actually asked for is carried
    // through here. Killing a loaded 200 GiB model because a daemon restarted would be
    // the wrong default.
    state.write(|app| {
        app.engine.shutdown_blocking_if_requested();
        if let Err(e) = app.profiles.save() {
            tracing::warn!("could not save profiles: {e:#}");
        }
    });
    tracing::info!("web interface stopped");
    Ok(())
}

/// Assemble the router. Split out so the tests can drive it without a socket.
pub fn router(state: Shared) -> Router {
    // Everything the browser needs before it has a token: the question, the answer, and
    // the page that asks it.
    let open = Router::new()
        .route("/api/auth", get(routes::meta::auth))
        .route("/api/login", post(routes::meta::login))
        .route("/api/logout", post(routes::meta::logout));

    let api = Router::new()
        .route("/api/snapshot", get(routes::meta::snapshot))
        .route("/api/events", get(events::events))
        .route("/api/knobs", get(routes::meta::knobs))
        .route("/api/confirm", post(routes::meta::confirm))
        .route("/api/logs", get(routes::streams::logs))
        .route("/api/logs/clear", post(routes::streams::clear_logs))
        .route("/api/requests", get(routes::streams::requests))
        .route("/api/requests/pause", post(routes::streams::pause_requests))
        .route("/api/requests/clear", post(routes::streams::clear_requests))
        .route("/api/jobs/{id}/output", get(routes::streams::job_output))
        .route("/api/engine/start", post(routes::engine::start))
        .route("/api/engine/stop", post(routes::engine::stop))
        .route("/api/engine/smoke-test", post(routes::engine::smoke_test))
        .route("/api/engine/summarize-upstream", post(routes::engine::summarize_upstream))
        .route("/api/freetoken/update", post(routes::engine::update_freetoken))
        .route("/api/models/rescan", post(routes::models::rescan))
        .route("/api/models/use", post(routes::models::use_model))
        .route("/api/models/convert", post(routes::models::convert))
        .route("/api/models/delete", post(routes::models::delete))
        .route("/api/models/sampling/apply", post(routes::models::apply_sampling))
        .route("/api/models/sampling/revert", post(routes::models::revert_sampling))
        .route("/api/hub/search", post(routes::hub::search))
        .route("/api/hub/open", post(routes::hub::open))
        .route("/api/hub/variant", post(routes::hub::variant))
        .route("/api/hub/files/toggle", post(routes::hub::toggle_file))
        .route("/api/hub/files/select", post(routes::hub::select_files))
        .route("/api/hub/download", post(routes::hub::download))
        .route("/api/hub/install-cli", post(routes::hub::install_cli))
        .route("/api/downloads/cancel", post(routes::hub::cancel_download))
        .route("/api/templates/preview", get(routes::templates::preview))
        .route("/api/templates/list-repo", post(routes::templates::list_repo))
        .route("/api/templates/fetch", post(routes::templates::fetch))
        .route("/api/templates/apply", post(routes::templates::apply))
        .route("/api/templates/revert", post(routes::templates::revert))
        .route("/api/templates/verify", post(routes::templates::verify))
        .route("/api/templates/delete", post(routes::templates::delete))
        .route("/api/serve/knob", post(routes::serve::knob))
        .route("/api/serve/flag", post(routes::serve::flag))
        .route("/api/serve/cycle", post(routes::serve::cycle))
        .route("/api/serve/plan", post(routes::serve::plan))
        .route("/api/serve/plan/apply", post(routes::serve::apply_plan))
        .route("/api/serve/plan/dismiss", post(routes::serve::dismiss_plan))
        .route("/api/profiles/save", post(routes::serve::save_profile))
        .route("/api/profiles/load", post(routes::serve::load_profile))
        .route("/api/profiles/delete", post(routes::serve::delete_profile))
        .route("/api/cache/pending", post(routes::cache::pending))
        .route("/api/cache/adjust", post(routes::cache::adjust))
        .route("/api/cache/reset-all", post(routes::cache::reset_all))
        .route("/api/cache/apply", post(routes::cache::apply))
        .route("/api/jobs/bench", post(routes::jobs::bench))
        .route("/api/jobs/cancel", post(routes::jobs::cancel))
        .route("/api/jobs/clear-finished", post(routes::jobs::clear_finished))
        .layer(axum::middleware::from_fn_with_state(state.clone(), auth::gate));

    // Applied to the open routes too: `POST /api/login` is the one request that turns a
    // browser into an authenticated one, so it is the one a hostile page most wants to
    // make on the operator's behalf.
    let guarded = open.merge(api).layer(axum::middleware::from_fn(guard::guard));

    guarded
        // An unknown path under /api is a 404 in the error envelope, never index.html: a
        // typo'd route that returns a page with status 200 is a debugging afternoon.
        .fallback(fallback)
        .with_state(state)
}

async fn fallback(
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if uri.path().starts_with("/api") {
        return state::ApiError::not_found("no such endpoint").into_response();
    }
    assets::serve(uri, headers).await
}

/// Drain the message channel into `App::handle`, exactly as the TUI's event loop does.
fn spawn_drain(state: Shared, mut rx: mpsc::UnboundedReceiver<Message>) {
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            state.write(|app| {
                app.handle(msg);
                // Drain anything else already queued so a burst costs one snapshot.
                while let Ok(next) = rx.try_recv() {
                    app.handle(next);
                }
            });
        }
    });
}

/// `App::tick` on the configured cadence: the same supervisor polling, toast expiry and
/// download rate sampling the terminal gets.
fn spawn_ticker(state: Shared, period: Duration) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            // Only wake the stream when the tick actually moved something: five identical
            // snapshots a second to every open browser is what this used to do, and it
            // left no second in which the heartbeat could fire.
            state.write_if(|app| {
                let changed = app.tick();
                ((), changed)
            });
        }
    });
}

/// Ctrl-C or a `systemctl stop`. Either way the engine keeps running.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!("could not listen for SIGTERM: {e}");
                std::future::pending::<()>().await;
            }
        }
    };
    tokio::select! {
        _ = ctrl_c => tracing::info!("received SIGINT; shutting down"),
        _ = terminate => tracing::info!("received SIGTERM; shutting down"),
    }
}
