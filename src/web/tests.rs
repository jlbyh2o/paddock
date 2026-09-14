//! Every route, driven against an `App` with no FreeToken — which is the state CI runs
//! in, and the one where most of the interesting refusals live.

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use super::auth::Auth;
use super::state::{Shared, WebState};
use crate::ui::app::App;
use crate::ui::smoke;

const TOKEN: &str = "s3cret-token";

async fn unguarded() -> Shared {
    WebState::new(fresh().await, Auth::new(None).unwrap())
}

async fn guarded() -> Shared {
    WebState::new(fresh().await, Auth::new(Some(TOKEN.into())).unwrap())
}

async fn fresh() -> App {
    smoke::app().await
}

async fn populated() -> Shared {
    let mut app = fresh().await;
    smoke::populate(&mut app);
    WebState::new(app, Auth::new(None).unwrap())
}

/// A populated daemon that can still receive its own messages.
///
/// `smoke::app` drops the receiving half, which is fine for routes that answer inline.
/// Anything that hands work to the blocking pool — sizing a checkpoint before asking to
/// delete it — answers through the channel instead, so those tests need to drain it.
async fn populated_with_inbox(
) -> (Shared, tokio::sync::mpsc::UnboundedReceiver<crate::ui::app::Message>) {
    let (mut app, rx) = smoke::app_with_inbox().await;
    smoke::populate(&mut app);
    (WebState::new(app, Auth::new(None).unwrap()), rx)
}

/// Deliver whatever a spawned task has sent back, as `web::spawn_drain` does.
async fn pump(
    state: &Shared,
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<crate::ui::app::Message>,
) {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    if let Ok(Some(msg)) = tokio::time::timeout_at(deadline, rx.recv()).await {
        state.write(|app| {
            app.handle(msg);
            while let Ok(next) = rx.try_recv() {
                app.handle(next);
            }
        });
    }
}

/// One request against a freshly assembled router, so no test can leak state into
/// another through a shared service.
async fn send(state: &Shared, req: Request<Body>) -> (StatusCode, Value) {
    let response = super::router(state.clone()).oneshot(req).await.expect("the router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 8 << 20).await.expect("a body");
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

async fn raw(state: &Shared, req: Request<Body>) -> (StatusCode, String, Option<String>) {
    let response = super::router(state.clone()).oneshot(req).await.expect("the router answers");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = axum::body::to_bytes(response.into_body(), 8 << 20).await.expect("a body");
    (status, String::from_utf8_lossy(&bytes).into_owned(), content_type)
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

fn post(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn with_bearer(mut req: Request<Body>, token: &str) -> Request<Body> {
    req.headers_mut().insert(header::AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
    req
}

fn with_cookie(mut req: Request<Body>, token: &str) -> Request<Body> {
    req.headers_mut().insert(header::COOKIE, format!("a=b; ft_man_token={token}").parse().unwrap());
    req
}

// ---------------------------------------------------------------- snapshot

#[tokio::test]
async fn a_snapshot_serializes_from_an_empty_app_and_a_populated_one() {
    for state in [unguarded().await, populated().await] {
        let (status, body) = send(&state, get("/api/snapshot")).await;
        assert_eq!(status, StatusCode::OK);
        let obj = body.as_object().expect("the snapshot is an object");
        for key in [
            "seq",
            "ts_ms",
            "version",
            "engine",
            "telemetry",
            "series",
            "hardware",
            "models",
            "hub",
            "templates",
            "serve",
            "cache",
            "jobs",
            "requests",
            "logs",
            "toasts",
            "confirm",
            "config",
            "environment",
        ] {
            assert!(obj.contains_key(key), "the snapshot must carry {key}");
        }
        assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));
        assert!(body["seq"].as_u64().unwrap() >= 1, "seq starts at 1");
    }
}

/// The whole point of the size budget: a populated machine's state has to fit in one
/// frame a browser can take ten times a second.
#[tokio::test]
async fn a_populated_snapshot_stays_inside_the_size_budget() {
    let state = populated().await;
    let (_, _, _) = raw(&state, get("/api/snapshot")).await;
    let (status, text, _) = raw(&state, get("/api/snapshot")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(text.len() < 64 * 1024, "the snapshot is {} bytes, over the 64 KiB budget", text.len());
}

#[tokio::test]
async fn seq_is_monotonic_across_snapshots() {
    let state = unguarded().await;
    let (_, first) = send(&state, get("/api/snapshot")).await;
    let (_, second) = send(&state, get("/api/snapshot")).await;
    assert!(second["seq"].as_u64().unwrap() > first["seq"].as_u64().unwrap());
}

/// Section 2.3 is the normative wire format, so it is asserted as exact JSON rather than
/// inspected field by field: the frontend is written against these shapes.
#[tokio::test]
async fn the_tagged_enum_encodings_match_the_contract() {
    use crate::ft::proc::{JobKind, JobStatus};
    use crate::ft::{EngineState, Preflight};
    use crate::models::Format;
    use crate::ui::widgets::{ConfirmAction, ToastKind};

    macro_rules! j {
        ($value:expr) => {
            serde_json::to_value(&$value).unwrap()
        };
    }

    assert_eq!(j!(EngineState::Stopped), serde_json::json!({"kind": "stopped"}));
    assert_eq!(j!(EngineState::Adopted), serde_json::json!({"kind": "adopted"}));
    assert_eq!(
        j!(EngineState::Exited { code: Some(1), signal: None }),
        serde_json::json!({"kind": "exited", "code": 1, "signal": null})
    );

    assert_eq!(j!(JobStatus::Running), serde_json::json!({"kind": "running"}));
    assert_eq!(j!(JobStatus::Canceled), serde_json::json!({"kind": "canceled"}));
    assert_eq!(
        j!(JobStatus::Failed("with status 1".to_string())),
        serde_json::json!({"kind": "failed", "reason": "with status 1"})
    );
    assert_eq!(
        j!(crate::hub::DownloadStatus::Failed("gone".to_string())),
        serde_json::json!({"kind": "failed", "reason": "gone"})
    );

    assert_eq!(
        j!(Preflight::Warn("expert_quant=none".to_string())),
        serde_json::json!({"kind": "warn", "detail": "expert_quant=none"})
    );

    assert_eq!(j!(crate::compat::Verdict::Unsupported), serde_json::json!("unsupported"));
    assert_eq!(j!(crate::compat::Verdict::Caution), serde_json::json!("caution"));
    assert_eq!(j!(Format::PartialFtw), serde_json::json!("partial_ftw"));
    assert_eq!(j!(Format::Hf), serde_json::json!("hf"));
    assert_eq!(j!(ToastKind::Warn), serde_json::json!("warn"));
    assert_eq!(j!(JobKind::Convert), serde_json::json!("convert"));
    assert_eq!(j!(crate::plan::Level::Advice), serde_json::json!("advice"));
    assert_eq!(j!(crate::ui::app::Pool::Moe), serde_json::json!("moe"));
    assert_eq!(j!(crate::variants::Role::Projector), serde_json::json!("projector"));
    assert_eq!(j!(crate::knobs::Group::Moe), serde_json::json!("moe"));

    assert_eq!(j!(crate::knobs::Kind::Text), serde_json::json!({"kind": "text"}));
    assert_eq!(
        j!(crate::knobs::Kind::Int { min: Some(1), max: None }),
        serde_json::json!({"kind": "int", "min": 1, "max": null})
    );
    assert_eq!(
        j!(crate::knobs::Kind::Float { min: 0.0, max: 1.0 }),
        serde_json::json!({"kind": "float", "min": 0.0, "max": 1.0})
    );
    assert_eq!(
        j!(crate::knobs::Kind::Choice(&["auto", "offload"])),
        serde_json::json!({"kind": "choice", "options": ["auto", "offload"]})
    );

    assert_eq!(
        j!(ConfirmAction::StopEngine { force: true }),
        serde_json::json!({"kind": "stop_engine", "force": true})
    );
    assert_eq!(
        j!(ConfirmAction::DeleteModel("/models/x".into())),
        serde_json::json!({"kind": "delete_model", "path": "/models/x"})
    );
    assert_eq!(j!(ConfirmAction::Quit), serde_json::json!({"kind": "quit"}));

    assert_eq!(
        j!(crate::templates::Status::BuiltIn),
        serde_json::json!({"kind": "built_in", "label": "built-in"})
    );
}

// ---------------------------------------------------------------- auth

#[tokio::test]
async fn auth_is_reported_and_never_gated() {
    let (status, body) = send(&unguarded().await, get("/api/auth")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"auth_required": false, "authorized": true}));

    let guarded = guarded().await;
    let (status, body) = send(&guarded, get("/api/auth")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"auth_required": true, "authorized": false}));

    let (_, body) = send(&guarded, with_bearer(get("/api/auth"), TOKEN)).await;
    assert_eq!(body["authorized"], true);
}

#[tokio::test]
async fn a_guarded_daemon_refuses_every_api_route_without_a_token() {
    let state = guarded().await;
    for req in [get("/api/snapshot"), get("/api/knobs"), get("/api/logs")] {
        let (status, body) = send(&state, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(body["error"].is_string(), "the error envelope is always used");
    }
    let (status, _) = send(&state, post("/api/models/rescan", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Section 1.3: the stream is gated like every other route, and an unauthorized request
/// must be answered as JSON *before* the response becomes an event stream. A `200
/// text/event-stream` that then carries an error frame is unparseable by `EventSource`,
/// which is why the frontend re-probes `/api/auth` on a stream error and needs a real 401
/// to find.
#[tokio::test]
async fn the_event_stream_is_a_json_401_before_it_is_a_stream() {
    let state = guarded().await;
    let (status, body, content_type) = raw(&state, get("/api/events")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    assert!(!body.contains("event:"), "no SSE bytes may precede the refusal: {body}");
    let parsed: Value = serde_json::from_str(&body).expect("the envelope is JSON");
    assert!(parsed["error"].is_string());

    // And with the cookie the browser actually sends, the stream opens.
    let response =
        super::router(state.clone()).oneshot(with_cookie(get("/api/events"), TOKEN)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
}

/// A cookie jar holds more than ours, and a pair with no `=` in it must not end the
/// search before `ft_man_token` is reached.
#[tokio::test]
async fn an_odd_cookie_beside_ours_does_not_hide_it() {
    let state = guarded().await;
    let mut req = get("/api/snapshot");
    req.headers_mut().insert(
        header::COOKIE,
        format!("consent; theme=dark; ft_man_token={TOKEN}").parse().unwrap(),
    );
    let (status, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn either_the_header_or_the_cookie_authorizes() {
    let state = guarded().await;
    let (status, _) = send(&state, with_bearer(get("/api/snapshot"), TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&state, with_cookie(get("/api/snapshot"), TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&state, with_bearer(get("/api/snapshot"), "wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logging_in_sets_a_session_cookie_and_a_wrong_token_does_not() {
    let state = guarded().await;
    let response = super::router(state.clone())
        .oneshot(post("/api/login", serde_json::json!({"token": TOKEN})))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(cookie.contains("ft_man_token="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(!cookie.contains("Max-Age"), "a session cookie has no Max-Age: {cookie}");

    let (status, body) = send(&state, post("/api/login", serde_json::json!({"token": "no"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "that token is not correct");

    // Logging out always succeeds, even with no token configured at all.
    let (status, body) = send(&unguarded().await, post("/api/logout", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authorized"], false);
}

// ---------------------------------------------------------------- static assets

#[tokio::test]
async fn the_spa_answers_every_path_that_is_not_an_api_route() {
    let state = unguarded().await;
    for path in ["/", "/models", "/some/client/route"] {
        let (status, body, content_type) = raw(&state, get(path)).await;
        assert_eq!(status, StatusCode::OK, "{path}");
        assert!(
            content_type.as_deref().is_some_and(|c| c.starts_with("text/html")),
            "{path} served {content_type:?}"
        );
        assert!(body.contains("<html") || body.contains("<!doctype"), "{path} served no page");
    }
}

#[tokio::test]
async fn an_unknown_api_path_is_a_json_404_not_the_index_page() {
    let (status, body, content_type) = raw(&unguarded().await, get("/api/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(content_type.as_deref(), Some("application/json"));
    assert!(!body.contains("<html"), "an API miss must never look like a page: {body}");
    let value: Value = serde_json::from_str(&body).unwrap();
    assert!(value["error"].is_string());
}

// ---------------------------------------------------------------- schema and streams

#[tokio::test]
async fn the_knob_schema_carries_every_group_and_knob() {
    let (status, body) = send(&unguarded().await, get("/api/knobs")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["groups"].as_array().unwrap().len(), crate::knobs::Group::ALL.len());
    assert_eq!(body["knobs"].as_array().unwrap().len(), crate::knobs::KNOBS.len());
    let first = &body["knobs"][0];
    assert_eq!(first["key"], "model");
    assert_eq!(first["flag"], "--model");
    assert!(first["kind"]["kind"].is_string());
}

#[tokio::test]
async fn the_log_stream_pages_by_sequence_and_survives_a_clear() {
    let state = populated().await;
    let (status, body) = send(&state, get("/api/logs?after=0&limit=5")).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    assert_eq!(items[0]["seq"], 1);
    assert!(items[0]["text"].is_string());
    assert_eq!(body["dropped"], 0);

    let next = body["next_after"].as_u64().unwrap();
    let (_, page) = send(&state, get(&format!("/api/logs?after={next}&limit=5"))).await;
    assert_eq!(page["items"][0]["seq"], next + 1);

    let last = body["last_seq"].as_u64().unwrap();
    let (status, cleared) = send(&state, post("/api/logs/clear", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cleared["status"], "ok");

    let (_, after) = send(&state, get("/api/logs")).await;
    assert!(after["items"].as_array().unwrap().is_empty());
    assert_eq!(after["last_seq"], last, "a clear does not renumber the stream");
    assert_eq!(after["first_seq"], last);
    assert!(after["dropped"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn the_request_stream_derives_a_decode_rate_and_pauses_on_request() {
    let state = populated().await;
    let (status, body) = send(&state, get("/api/requests?after=0&limit=3")).await;
    assert_eq!(status, StatusCode::OK);
    let first = &body["items"][0];
    assert_eq!(first["seq"], 1);
    assert_eq!(first["method"], "POST");
    assert!(first["decode_tps"].as_f64().unwrap() > 0.0);

    let (status, body) =
        send(&state, post("/api/requests/pause", serde_json::json!({"paused": true}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["paused"], true);
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["requests"]["paused"], true);

    let (status, body) = send(&state, post("/api/requests/clear", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["requests"]["count"], 0);
}

#[tokio::test]
async fn a_jobs_output_page_needs_a_real_job() {
    let (status, body) = send(&unguarded().await, get("/api/jobs/999/output")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("999"));
}

/// Section 2.14: `output_seq` is the change counter for `GET /api/jobs/{id}/output`.
/// Without it the browser had no way to know a job had written anything and polled on a
/// timer; with it a client fetches when the number moves and not otherwise. It is the
/// job's own ring counter, so no snapshot stats a file.
#[tokio::test]
async fn a_job_reports_its_output_line_counter() {
    let state = unguarded().await;
    state.write(|app| {
        let job = crate::ft::Job::fake(
            crate::ft::proc::JobKind::Convert,
            "measurable",
            crate::ft::proc::JobStatus::Running,
            crate::ft::proc::JobProgress::default(),
        );
        job.log.clear();
        job.log.push("one line".into(), false);
        app.jobs = vec![job];
    });

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    let first = snap["jobs"]["items"][0]["output_seq"].as_u64().expect("a counter");
    assert!(first >= 1);

    state.write(|app| app.jobs[0].log.push("and another".into(), false));
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(
        snap["jobs"]["items"][0]["output_seq"],
        first + 1,
        "a line written moves the counter"
    );

    // A clear does not renumber, so a client holding the old value still sees it move.
    state.write(|app| app.jobs[0].log.clear());
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["jobs"]["items"][0]["output_seq"], first + 1);
}

#[tokio::test]
async fn a_jobs_output_page_reports_a_missing_log_rather_than_panicking() {
    let state = populated().await;
    let id = state.read(|app| app.jobs[0].id);
    let (status, _) = send(&state, get(&format!("/api/jobs/{id}/output"))).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

// ---------------------------------------------------------------- confirmations

#[tokio::test]
async fn confirming_nothing_is_a_conflict() {
    let (status, body) =
        send(&unguarded().await, post("/api/confirm", serde_json::json!({"accept": true}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no confirmation is pending");
}

/// The confirmation round trip a browser actually makes: an action that would delete
/// something returns `confirm_pending`, the modal arrives in the snapshot, and nothing
/// has happened until it is answered.
#[tokio::test]
async fn a_destructive_action_confirms_before_it_acts() {
    let (state, mut rx) = populated_with_inbox().await;
    let path = state.read(|app| app.models[0].path.display().to_string());

    // Sizing the checkpoint runs on the blocking pool, so the route reports that work has
    // started and the modal arrives with the next snapshot rather than in the reply.
    let (status, body) =
        send(&state, post("/api/models/delete", serde_json::json!({"path": path}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "started");
    pump(&state, &mut rx).await;

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    let confirm = &snap["confirm"];
    assert_eq!(confirm["title"], "Delete checkpoint");
    assert_eq!(confirm["destructive"], true);
    assert_eq!(confirm["default_index"], 0);
    assert_eq!(confirm["action"]["kind"], "delete_model");

    // Dismissing clears it and does nothing else.
    let (status, body) =
        send(&state, post("/api/confirm", serde_json::json!({"accept": false}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert!(snap["confirm"].is_null());
}

// ---------------------------------------------------------------- engine

#[tokio::test]
async fn engine_routes_refuse_without_freetoken_or_a_running_engine() {
    let state = unguarded().await;

    let (status, body) = send(&state, post("/api/engine/start", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(body["error"].is_string());

    let (status, body) =
        send(&state, post("/api/engine/stop", serde_json::json!({"force": false}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no engine is running");

    let (status, body) = send(&state, post("/api/engine/smoke-test", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "the server is not answering");
}

/// A live engine is what makes a stop meaningful, and the confirmation is what makes it
/// safe. Both halves are checked here because neither is reachable from the empty state.
#[tokio::test]
async fn stopping_a_live_engine_confirms_first() {
    let state = populated().await;
    state.write(|app| app.engine.state = crate::ft::EngineState::Running);

    let (status, body) =
        send(&state, post("/api/engine/stop", serde_json::json!({"force": true}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "confirm_pending");

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["confirm"]["title"], "Force-stop the engine");
    assert_eq!(
        snap["confirm"]["action"],
        serde_json::json!({"kind": "stop_engine", "force": true})
    );

    // And an engine already running is what refuses a start, before the missing CLI does.
    let (status, body) = send(&state, post("/api/engine/start", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "an engine is already running; stop it first");
}

// ---------------------------------------------------------------- models

#[tokio::test]
async fn a_rescan_always_starts() {
    let (status, body) =
        send(&unguarded().await, post("/api/models/rescan", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "started");
}

#[tokio::test]
async fn an_unknown_model_path_is_a_404_on_every_model_route() {
    let state = populated().await;
    let body = serde_json::json!({"path": "/models/not-here"});
    for route in ["/api/models/use", "/api/models/convert", "/api/models/delete"] {
        let (status, reply) = send(&state, post(route, body.clone())).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
        assert_eq!(reply["error"], "that model is no longer in the library");
    }
}

#[tokio::test]
async fn using_a_model_prefers_its_ftw_build_and_sets_the_served_name() {
    let state = populated().await;
    let (status, body) = send(
        &state,
        post("/api/models/use", serde_json::json!({"path": "/models/Qwen3.6-35B-A3B"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["serve"]["values"]["model"], "/models/Qwen3.6-35B-A3B-ftw");
    assert_eq!(snap["serve"]["values"]["served_model_name"], "Qwen3.6-35B-A3B");
}

#[tokio::test]
async fn converting_an_ftw_build_is_refused_as_pointless() {
    let state = populated().await;
    let (status, body) = send(
        &state,
        post("/api/models/convert", serde_json::json!({"path": "/models/Qwen3.6-35B-A3B-ftw"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("already in FTW format"), "{body}");
}

// ---------------------------------------------------------------- hub

#[tokio::test]
async fn an_empty_hub_search_is_a_400() {
    let (status, body) =
        send(&unguarded().await, post("/api/hub/search", serde_json::json!({"query": "  "}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "a search needs a query");
}

#[tokio::test]
async fn opening_a_repo_starts_the_listing_and_the_compatibility_check() {
    let state = unguarded().await;
    let (status, body) =
        send(&state, post("/api/hub/open", serde_json::json!({"repo_id": "Qwen/Qwen3.6-35B-A3B"})))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "started");
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["hub"]["loading_info"], true);
}

#[tokio::test]
async fn hub_file_selection_is_by_path_and_reports_what_changed() {
    let state = populated().await;
    let (status, body) = send(
        &state,
        post("/api/hub/files/toggle", serde_json::json!({"path": "config.json", "wanted": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "ok", "wanted": false}));

    let (status, body) =
        send(&state, post("/api/hub/files/toggle", serde_json::json!({"path": "no-such-file"})))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");

    let (status, body) =
        send(&state, post("/api/hub/files/select", serde_json::json!({"mode": "all"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["selected_count"].as_u64().unwrap() > 0);

    let (_, body) =
        send(&state, post("/api/hub/files/select", serde_json::json!({"mode": "none"}))).await;
    assert_eq!(body["selected_count"], 0);

    // With nothing selected there is nothing to download.
    let (status, body) = send(&state, post("/api/hub/download", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no files selected");
}

/// The quantization list is the Hub tab's whole reason for existing, so the shape it
/// reaches the browser in is asserted rather than assumed: `file_count` is derived and
/// was missing from an earlier flattened form of this.
#[tokio::test]
async fn a_multi_quantization_layout_reaches_the_snapshot_whole() {
    let state = unguarded().await;
    let siblings = vec![
        crate::hub::Sibling { path: "config.json".into(), size: Some(1400) },
        crate::hub::Sibling { path: "UD-IQ3_XXS/model-00001-of-00002.gguf".into(), size: Some(9) },
        crate::hub::Sibling { path: "UD-IQ3_XXS/model-00002-of-00002.gguf".into(), size: Some(9) },
        crate::hub::Sibling { path: "Q8_0/model.gguf".into(), size: Some(18) },
    ];
    state.write(|app| app.hub_view.layout = crate::variants::analyze(&siblings));

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    let layout = &snap["hub"]["layout"];
    assert_eq!(layout["is_multi"], true);
    let variants = layout["variants"].as_array().expect("variants are an array");
    let ud = variants.iter().find(|v| v["label"] == "UD-IQ3_XXS").expect("the repo offers it");
    assert_eq!(ud["role"], "weights");
    assert_eq!(ud["file_count"], 2, "the browser must not have to count files itself");
    assert_eq!(ud["subdir"], "UD-IQ3_XXS");
    assert!(layout["shared"].as_array().unwrap().iter().any(|f| f == "config.json"));

    // Choosing one applies its files to the selection and says how much it is.
    let (status, body) =
        send(&state, post("/api/hub/variant", serde_json::json!({"label": "Q8_0"}))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["hub"]["variant"], "Q8_0");
    assert_eq!(snap["hub"]["custom_selection"], false);
}

#[tokio::test]
async fn choosing_an_absent_variant_is_a_conflict() {
    let state = populated().await;
    let (status, body) =
        send(&state, post("/api/hub/variant", serde_json::json!({"label": "UD-IQ3_XXS"}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("UD-IQ3_XXS"));
}

#[tokio::test]
async fn downloading_before_a_repo_is_open_says_to_open_one() {
    let (status, body) =
        send(&unguarded().await, post("/api/hub/download", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "select a repo and press Enter to list its files first");
}

#[tokio::test]
async fn canceling_an_unknown_download_is_a_404() {
    let (status, body) =
        send(&unguarded().await, post("/api/downloads/cancel", serde_json::json!({"id": 42})))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("42"));
}

#[tokio::test]
async fn installing_the_hf_cli_confirms_or_says_it_is_already_there() {
    let state = unguarded().await;
    let already = state.read(|app| app.hf_cli.is_some());
    let (status, body) = send(&state, post("/api/hub/install-cli", serde_json::json!({}))).await;
    if already {
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"], "the hf CLI is already installed");
    } else {
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["status"], "confirm_pending");
        let (_, snap) = send(&state, get("/api/snapshot")).await;
        assert_eq!(snap["confirm"]["action"]["kind"], "install_hf_cli");
    }
}

// ---------------------------------------------------------------- templates

#[tokio::test]
async fn template_routes_refuse_unknown_identities() {
    let state = populated().await;

    let (status, _) =
        send(&state, post("/api/templates/list-repo", serde_json::json!({"repo": ""}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = send(
        &state,
        post(
            "/api/templates/apply",
            serde_json::json!({"template": "nope", "model_path": "/models/Qwen3.6-35B-A3B"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "no template named 'nope'");

    let (status, body) = send(
        &state,
        post("/api/templates/apply", serde_json::json!({"template": "Qwen-Sharp-Chat-Templates", "model_path": "/models/gone"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "that model is no longer in the library");

    let (status, body) = send(
        &state,
        post("/api/templates/verify", serde_json::json!({"template": "Qwen-Sharp-Chat-Templates", "model_path": "/models/Qwen3.6-35B-A3B"})),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");

    let (status, body) = send(
        &state,
        post("/api/templates/revert", serde_json::json!({"model_path": "/models/Qwen3.6-35B-A3B"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().contains("not using an ft-man template override"));

    let (status, _) =
        send(&state, post("/api/templates/delete", serde_json::json!({"name": "nope"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = send(&state, get("/api/templates/preview?name=nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // With no listing yet there is no commit to fetch at, and guessing one would fetch
    // whatever `main` happens to be hours later.
    let (status, body) = send(
        &unguarded().await,
        post("/api/templates/fetch", serde_json::json!({"repo": "org/t", "path": "a.jinja"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // The populated fixture has listed a repo, so the same body resolves and starts.
    let (status, body) = send(
        &state,
        post("/api/templates/fetch", serde_json::json!({"repo": "org/t", "path": "a.jinja"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "started");
}

/// A template that really is in the store, read back through the route the preview pane
/// uses.
#[tokio::test]
async fn a_stored_template_previews_and_refreshes_the_shared_cache() {
    let state = unguarded().await;
    let jinja = "{%- set template_version = \"v1\" %}\n{{ messages }}";
    crate::templates::save("web-preview-fixture", jinja, Default::default()).unwrap();
    state.write(|app| app.reload_templates());

    let (status, body) = send(&state, get("/api/templates/preview?name=web-preview-fixture")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "web-preview-fixture");
    assert_eq!(body["text"], jinja);
    assert_eq!(body["truncated"], false);

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["templates"]["preview"]["name"], "web-preview-fixture");

    let (status, body) = send(
        &state,
        post("/api/templates/delete", serde_json::json!({"name": "web-preview-fixture"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "confirm_pending");
    let (status, _) = send(&state, post("/api/confirm", serde_json::json!({"accept": true}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(crate::templates::get("web-preview-fixture").is_none());
}

// ---------------------------------------------------------------- serve

#[tokio::test]
async fn setting_a_knob_validates_clears_exclusions_and_unsets_on_null() {
    let state = unguarded().await;

    let (status, body) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": "1.5"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "--memory-ratio: must be between 0.05 and 1");

    let (status, body) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": "0.85"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "ok", "set": true, "cleared": []}));

    // Setting one MoE sizing knob clears the others it excludes, and says which.
    send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "moe_cache_rate", "value": "0.5"})),
    )
    .await;
    let (_, body) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "moe_cache_size", "value": "512"})),
    )
    .await;
    assert_eq!(body["cleared"], serde_json::json!(["moe_cache_rate"]));

    let (status, body) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": null})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["set"], false);

    let (status, body) =
        send(&state, post("/api/serve/knob", serde_json::json!({"key": "nope", "value": "1"})))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "no knob named 'nope'");
}

/// Section 4.8 and the exception in 1.2: a rejected knob value is the one refusal that
/// pushes no toast. The browser renders it inline against the field, and two renderings of
/// one problem is what the rule exists to prevent. The terminal still toasts, from
/// `input::commit_knob_edit`, because it has no inline slot.
#[tokio::test]
async fn a_rejected_knob_value_is_shown_inline_and_never_toasted() {
    let state = unguarded().await;
    let (status, body) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": "1.5"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "--memory-ratio: must be between 0.05 and 1");

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(
        snap["toasts"],
        serde_json::json!([]),
        "a validation failure has an inline slot in the browser; it must not also toast"
    );

    // Every other refusal still does, so the rule stays one exception rather than a drift.
    let (status, _) = send(&state, post("/api/engine/stop", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["toasts"].as_array().map(Vec::len), Some(1));
}

#[tokio::test]
async fn flags_toggle_and_choices_cycle_through_unset() {
    let state = unguarded().await;

    let (status, body) =
        send(&state, post("/api/serve/flag", serde_json::json!({"key": "moe_cache_auto"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, serde_json::json!({"status": "ok", "on": true}));
    let (_, body) = send(
        &state,
        post("/api/serve/flag", serde_json::json!({"key": "moe_cache_auto", "on": false})),
    )
    .await;
    assert_eq!(body["on"], false);

    let (status, body) =
        send(&state, post("/api/serve/flag", serde_json::json!({"key": "memory_ratio"}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let mut seen = Vec::new();
    for _ in 0..6 {
        let (status, body) = send(
            &state,
            post("/api/serve/cycle", serde_json::json!({"key": "moe_strategy", "delta": 1})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        seen.push(body["value"].clone());
    }
    assert_eq!(seen[0], "auto");
    assert_eq!(seen[4], "fused");
    assert!(seen[5].is_null(), "stepping off the end returns to the default");

    let (status, _) =
        send(&state, post("/api/serve/cycle", serde_json::json!({"key": "model"}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn planning_needs_a_model_and_the_overlay_can_be_dismissed() {
    let state = unguarded().await;
    let (status, body) = send(&state, post("/api/serve/plan", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body["error"].as_str().unwrap().starts_with("cannot plan: no model is configured"));

    let (status, body) = send(&state, post("/api/serve/plan/apply", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "no plan is held");

    let (status, body) = send(&state, post("/api/serve/plan/dismiss", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn a_plan_builds_applies_and_reports_how_many_knobs_moved() {
    let state = populated().await;
    state.write(|app| app.serve.set("model", "/models/Qwen3.6-35B-A3B"));

    let (status, body) = send(&state, post("/api/serve/plan", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["plan"], true);

    let (_, snap) = send(&state, get("/api/snapshot")).await;
    let plan = &snap["serve"]["plan"];
    assert!(!plan["steps"].as_array().unwrap().is_empty());
    assert!(plan["edit_count"].as_u64().unwrap() > 0);
    assert!(plan["steps"][0]["label"].is_string());

    let (status, body) = send(&state, post("/api/serve/plan/apply", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["changed"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn profiles_save_load_and_delete_by_name() {
    let state = unguarded().await;
    state.write(|app| app.serve.set("moe_strategy", "cpu"));

    let (status, _) =
        send(&state, post("/api/profiles/save", serde_json::json!({"name": "  "}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) =
        send(&state, post("/api/profiles/save", serde_json::json!({"name": "web-test"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["created"], true);
    let (_, body) =
        send(&state, post("/api/profiles/save", serde_json::json!({"name": "web-test"}))).await;
    assert_eq!(body["created"], false, "a second save updates rather than creates");

    state.write(|app| app.serve = crate::knobs::ServeConfig::new());
    let (status, body) =
        send(&state, post("/api/profiles/load", serde_json::json!({"name": "web-test"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["serve"]["values"]["moe_strategy"], "cpu");

    let (status, _) =
        send(&state, post("/api/profiles/load", serde_json::json!({"name": "nope"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, body) =
        send(&state, post("/api/profiles/delete", serde_json::json!({"name": "web-test"}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "confirm_pending");
    send(&state, post("/api/confirm", serde_json::json!({"accept": true}))).await;
    let (status, _) =
        send(&state, post("/api/profiles/delete", serde_json::json!({"name": "web-test"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------- cache

#[tokio::test]
async fn cache_routes_need_a_live_geometry() {
    let state = unguarded().await;
    for (route, body) in [
        ("/api/cache/pending", serde_json::json!({"pool": "kv", "value": 8192})),
        ("/api/cache/adjust", serde_json::json!({"pool": "kv", "percent": 0.01})),
        ("/api/cache/apply", serde_json::json!({})),
    ] {
        let (status, reply) = send(&state, post(route, body)).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{route}");
        assert_eq!(reply["error"], "cache geometry is only available while the engine is serving");
    }
    // Clearing every pending edit is meaningful with or without a geometry.
    let (status, body) = send(&state, post("/api/cache/reset-all", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn a_pool_the_geometry_does_not_expose_is_a_404() {
    let state = populated().await;
    let (status, body) =
        send(&state, post("/api/cache/pending", serde_json::json!({"pool": "mamba", "value": 4})))
            .await;
    assert_eq!(status, StatusCode::OK, "the fixture does expose GDN slots: {body}");

    state.write(|app| {
        if let Some(c) = app.telemetry.cache.as_mut() {
            c.geometry.num_mamba_slots = 0;
        }
    });
    let (status, body) =
        send(&state, post("/api/cache/pending", serde_json::json!({"pool": "mamba", "value": 4})))
            .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn staging_a_pool_edit_clamps_adjusts_and_applies_through_a_confirmation() {
    let state = populated().await;

    let (status, body) =
        send(&state, post("/api/cache/pending", serde_json::json!({"pool": "kv", "value": 65536})))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pending"], 65536);

    // The snapshot does the pool arithmetic so the browser never has to.
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    let kv = snap["cache"]["pools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["pool"] == "kv")
        .expect("KV is a present pool");
    assert_eq!(kv["pending"], 65536);
    assert_eq!(kv["shown"], 65536);
    assert_eq!(kv["delta"], 65536i64 - 131_072);
    assert_eq!(kv["unit"], "pages");
    assert!(snap["cache"]["proposed_bytes"].as_u64().unwrap() > 0);
    assert!(snap["cache"]["delta_bytes"].as_i64().unwrap() < 0);

    // Back to where it started is not an edit.
    let (_, body) = send(
        &state,
        post("/api/cache/pending", serde_json::json!({"pool": "kv", "value": 131072})),
    )
    .await;
    assert!(body["pending"].is_null());

    let (status, body) =
        send(&state, post("/api/cache/adjust", serde_json::json!({"pool": "moe", "percent": 0.1})))
            .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["pending"].as_u64().unwrap() > 512);

    let (status, body) = send(&state, post("/api/cache/apply", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "confirm_pending");
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["confirm"]["title"], "Rebuild cache");
    send(&state, post("/api/confirm", serde_json::json!({"accept": false}))).await;

    send(&state, post("/api/cache/reset-all", serde_json::json!({}))).await;
    let (status, body) = send(&state, post("/api/cache/apply", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "nothing to apply");
}

// ---------------------------------------------------------------- jobs

#[tokio::test]
async fn benching_without_freetoken_is_a_503() {
    let (status, body) =
        send(&unguarded().await, post("/api/jobs/bench", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn cancelling_and_clearing_jobs_work_on_ids_not_rows() {
    let state = populated().await;
    let (status, body) =
        send(&state, post("/api/jobs/cancel", serde_json::json!({"id": 9999}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body["error"].as_str().unwrap().contains("9999"));

    let (running, finished) = state.read(|app| {
        let running = app.jobs.iter().find(|j| j.is_running()).map(|j| j.id).unwrap();
        let finished = app.jobs.iter().find(|j| !j.is_running()).map(|j| j.id).unwrap();
        (running, finished)
    });

    let (status, body) =
        send(&state, post("/api/jobs/cancel", serde_json::json!({"id": finished}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "that job has already finished");

    let (status, body) =
        send(&state, post("/api/jobs/cancel", serde_json::json!({"id": running}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "confirm_pending");
    send(&state, post("/api/confirm", serde_json::json!({"accept": false}))).await;

    let (status, body) =
        send(&state, post("/api/jobs/clear-finished", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["removed"], 1);
}

// ---------------------------------------------------------------- events

/// The one thing every client depends on: whatever else arrives, a snapshot arrives
/// first.
#[tokio::test]
async fn the_event_stream_opens_with_a_snapshot() {
    let state = unguarded().await;
    let response = super::router(state.clone()).oneshot(get("/api/events")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );

    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    // Read until the first snapshot frame is complete, or give up rather than hang.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !text.contains("event: snapshot") || !text.trim_end().ends_with('}') {
        let next =
            tokio::time::timeout_at(deadline, futures_util::StreamExt::next(&mut body)).await;
        match next {
            Ok(Some(Ok(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
            _ => break,
        }
    }

    assert!(text.contains("retry: 2000"), "the retry interval is sent once, first: {text:.200}");
    assert!(text.contains("event: snapshot"), "the first frame is a snapshot: {text:.200}");
    let data = text
        .lines()
        .find_map(|l| l.strip_prefix("data: "))
        .expect("the snapshot frame carries its data");
    let parsed: Value = serde_json::from_str(data).expect("the frame is one JSON line");
    assert!(parsed["seq"].as_u64().unwrap() >= 1);
    assert_eq!(parsed["version"], env!("CARGO_PKG_VERSION"));
}

// ---------------------------------------------------------------- wire fixtures

/// The frontend's wire tests read real documents rather than a hand-written imitation of
/// one, so the two halves of the contract are compared mechanically. This test asserts
/// the documents parse; with `FT_MAN_DUMP_SNAPSHOTS=1` it also rewrites the fixtures
/// under `web/src/mock/`, which is how they are regenerated after a shape change.
#[tokio::test]
async fn the_wire_fixtures_the_frontend_reads_are_this_serialization() {
    let dump = std::env::var("FT_MAN_DUMP_SNAPSHOTS").is_ok_and(|v| v == "1");
    let mock = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("web/src/mock");

    let cases: [(&str, Shared); 2] = [
        ("snapshot.populated.json", wire_populated().await),
        ("snapshot.empty.json", unguarded().await),
    ];
    for (name, state) in cases {
        let (status, body) = send(&state, get("/api/snapshot")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.is_object(), "{name} is an object");
        write_fixture(dump, &mock.join(name), &body);
    }

    let state = unguarded().await;
    let (status, knobs) = send(&state, get("/api/knobs")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(knobs["knobs"].as_array().is_some_and(|k| !k.is_empty()), "the schema is non-empty");
    write_fixture(dump, &mock.join("knobs.json"), &knobs);

    // Whether or not this run rewrote them, the committed fixtures must not name whoever
    // generated them. They are checked in, read by the frontend's tests, and published with
    // the repository.
    if dump {
        for name in ["snapshot.populated.json", "snapshot.empty.json", "knobs.json"] {
            let path = mock.join(name);
            let text = std::fs::read_to_string(&path).expect("a fixture to re-read");
            for leak in identity_terms() {
                assert!(
                    !text.to_lowercase().contains(&leak.to_lowercase()),
                    "{name} names the machine it was generated on: {leak}"
                );
            }
        }
    }
}

/// Strings that would identify whoever ran the dump. The home directory and the host name
/// reach the snapshot through real fields — `config.download_dir`, `models.roots`,
/// `hardware.host.hostname` — so they are rewritten rather than omitted, and the fixture
/// keeps the shape a real machine produces.
fn identity_terms() -> Vec<String> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(str::to_string)) {
        out.push(home.clone());
        // The user name on its own, which also appears inside a host name like `box-alice`.
        if let Some(user) = home.rsplit('/').next().filter(|u| u.len() > 2) {
            out.push(user.to_string());
        }
    }
    if let Ok(host) = hostname_of_this_machine() {
        out.push(host);
    }
    out
}

fn hostname_of_this_machine() -> Result<String, ()> {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|h| h.trim().to_string())
        .map_err(|_| ())
        .and_then(|h| if h.is_empty() { Err(()) } else { Ok(h) })
}

/// Write one fixture, with this machine's identity replaced by a fixed stand-in.
///
/// The frontend's wire tests read these documents and the repository ships them, so a dump
/// taken on a developer's laptop must not carry that laptop's home directory or host name
/// into the tree. The substitution is textual and value-only: every key, every shape and
/// every path *structure* is exactly what the daemon serialized, which is the whole reason
/// these are generated rather than written by hand.
fn write_fixture(dump: bool, path: &std::path::Path, value: &Value) {
    if !dump {
        return;
    }
    let mut text = serde_json::to_string_pretty(value).expect("a fixture serializes");
    if let Some(home) = dirs::home_dir().and_then(|h| h.to_str().map(str::to_string)) {
        text = text.replace(&home, "/home/user");
    }
    if let Ok(host) = hostname_of_this_machine() {
        text = text.replace(&host, "gpu-box");
    }
    text.push('\n');
    std::fs::write(path, text).unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}

/// `smoke::populate` fills the panes a terminal draws, but several wire shapes are only
/// reachable through an action — a pending confirmation, a built plan, a render check, a
/// multi-quantization layout. The frontend fixture has to carry them, because a field the
/// populated document never produces is a field neither side can be checked against. So
/// the dump starts from `populate` and then puts every optional structure into state.
async fn wire_populated() -> Shared {
    let mut app = fresh().await;
    smoke::populate(&mut app);

    app.info("engine started");
    app.warn("the hf CLI is not installed");

    app.confirm = Some(crate::ui::widgets::Confirm::new(
        "Delete checkpoint",
        vec!["/models/Qwen3.6-35B-A3B".into(), String::new(), "frees 21.0 GiB".into()],
        crate::ui::widgets::ConfirmAction::DeleteModel("/models/Qwen3.6-35B-A3B".into()),
        true,
    ));

    app.hub_view.layout = crate::variants::analyze(&[
        crate::hub::Sibling { path: "config.json".into(), size: Some(1400) },
        crate::hub::Sibling { path: "UD-IQ3_XXS/model-00001-of-00002.gguf".into(), size: Some(9) },
        crate::hub::Sibling { path: "Q8_0/model.gguf".into(), size: Some(18) },
    ]);
    app.hub_view.variant = Some("Q8_0".into());

    app.templates_view.preflight =
        Some(("qwen-sharp".into(), crate::ft::Preflight::Ok("renders in 4 ms".into())));

    // The probe runs a real `ft --version`, which this test has no business doing, so the
    // line the Engine pane draws is seeded rather than read.
    app.set_ft_version(Some("freetoken version 0.1.2".into()));

    // A checkout behind upstream, because that is the state the Engine pane has something
    // to say about. Reading a real one here would make the fixture depend on whichever
    // tree the machine running the tests happens to have.
    app.set_ft_checkout(Some(crate::ft::FtCheckout {
        path: "/home/user/FreeToken".into(),
        upstream: "https://github.com/FlashML-org/FreeToken.git".into(),
        origin: "https://github.com/user/FreeToken.git".into(),
        local_sha: "9f8e7d6".into(),
        upstream_sha: "a1b2c3d".into(),
        origin_sha: "9f8e7d6".into(),
        origin_ahead: 0,
        origin_behind: 3,
        upstream_behind: 3,
        dirty: false,
        kernels_stale: Some(false),
    }));

    if let Some(health) = app.telemetry.health.as_mut() {
        health.phase = Some("experts".into());
        health.progress = Some(crate::ft::types::LoadProgress {
            done_bytes: 12_884_901_888,
            total_bytes: 22_548_578_304,
        });
    }

    // Name the serve after the model the engine reports, so the live geometry prices the
    // plan and `plan.fit` is a document rather than a null.
    app.serve.set("served_model_name", "Qwen3.6-35B-A3B");
    // Two knobs the schema declares mutually exclusive, so `serve.errors` carries the
    // shape the Serve tab renders inline. `ServeConfig::set` clears exclusions, so the
    // clash is reached the way a real one is: a profile written before they collided.
    let mut values = serde_json::to_value(&app.serve).expect("the serve config serializes");
    values["num_pages"] = "16384".into();
    values["num_tokens"] = "262144".into();
    app.serve = serde_json::from_value(values).expect("the serve config round-trips");
    app.serve_view.plan = crate::ui::views::plan::build(&app).ok();

    // The cached samples a live daemon fills in on its first hardware tick and first
    // scan. Without them `config.disk_free`, `hub.disk_free` and `models.roots` are
    // null or empty, and the frontend's key comparison could never see their shape.
    app.model_roots = vec![
        crate::ui::app::Root { path: "/models".into(), exists: true },
        crate::ui::app::Root { path: "/srv/missing".into(), exists: false },
    ];
    app.disk_free_download = Some(("/models".into(), 512 * (1 << 30)));
    app.disk_free_target = Some(("/models".into(), 512 * (1 << 30)));

    WebState::new(app, Auth::new(None).unwrap())
}

// ---------------------------------------------------------------- request guard

fn with_header(mut req: Request<Body>, name: &str, value: &str) -> Request<Body> {
    req.headers_mut().insert(
        axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
        value.parse().unwrap(),
    );
    req
}

/// The shape a cross-site request forgery takes against this daemon: a page in the
/// operator's browser `fetch`es the LAN address, the browser attaches the session cookie,
/// and a delete runs. The `Origin` header is the one thing the attacking page cannot forge.
#[tokio::test]
async fn a_cross_origin_state_change_is_refused() {
    let state = populated().await;
    let req = with_header(
        with_header(post("/api/models/rescan", serde_json::json!({})), "host", "box.lan:7979"),
        "origin",
        "http://evil.example",
    );
    let (status, body) = send(&state, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body["error"].as_str().unwrap().contains("evil.example"), "{body}");
    assert_eq!(body["toasted"], false, "the daemon refused before any action ran");

    // The daemon's own page is allowed, on either scheme.
    for origin in ["http://box.lan:7979", "https://box.lan:7979"] {
        let req = with_header(
            with_header(post("/api/models/rescan", serde_json::json!({})), "host", "box.lan:7979"),
            "origin",
            origin,
        );
        let (status, _) = send(&state, req).await;
        assert_eq!(status, StatusCode::OK, "{origin} is this daemon's own origin");
    }

    // No Origin at all is `curl`, which has no cookie jar to borrow.
    let (status, _) = send(&state, post("/api/models/rescan", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK);

    // And a read is never refused: the browser's own rules keep another origin from
    // seeing the response.
    let req = with_header(get("/api/snapshot"), "origin", "http://evil.example");
    let (status, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
}

/// A cross-site form post needs no preflight at all, and it can only send one of three
/// content types — none of them JSON. Requiring JSON puts every state-changing route
/// behind a preflight the browser will refuse to make on a hostile page's behalf.
#[tokio::test]
async fn a_form_shaped_body_is_refused_with_415() {
    let state = unguarded().await;
    for content_type in
        ["application/x-www-form-urlencoded", "multipart/form-data; boundary=x", "text/plain"]
    {
        let req = Request::builder()
            .method("POST")
            .uri("/api/models/rescan")
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from("{}"))
            .unwrap();
        let (status, body) = send(&state, req).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{content_type}");
        assert!(body["error"].as_str().unwrap().contains("application/json"), "{body}");
    }

    // A body-less POST sends no Content-Type at all, and half these routes take none.
    let req =
        Request::builder().method("POST").uri("/api/models/rescan").body(Body::empty()).unwrap();
    let (status, _) = send(&state, req).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------- error envelope

/// Section 1.2: the envelope says whether a toast is also on its way, so a client renders
/// one problem rather than two.
#[tokio::test]
async fn the_error_envelope_says_whether_the_refusal_also_toasted() {
    let state = unguarded().await;

    // Field-shaped: the browser has an inline slot under the value, so no toast.
    let (status, body) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": "1.5"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["toasted"], false);

    // An identity that no longer exists is about the request too.
    let (status, body) =
        send(&state, post("/api/serve/knob", serde_json::json!({"key": "nope"}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["toasted"], false);

    // A refusal about the state of the machine reaches the terminal as a toast, and the
    // browser is told so rather than rendering the same sentence twice.
    let (status, body) = send(&state, post("/api/engine/stop", serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["toasted"], true);
    let (_, snap) = send(&state, get("/api/snapshot")).await;
    assert_eq!(snap["toasts"].as_array().map(Vec::len), Some(1));

    // And a refusal the web layer itself raised toasted nothing: there was no action.
    let (status, body) = send(&state, get("/api/nope")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["toasted"], false);
}

/// A refusal that toasts nothing must also not wake every other connected browser: there
/// is nothing new for them to render.
#[tokio::test]
async fn a_silent_refusal_publishes_no_snapshot() {
    let state = unguarded().await;
    let before = *state.changed.borrow();
    let (status, _) = send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": "1.5"})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(*state.changed.borrow(), before, "nothing changed, so nothing was published");

    // The accepted value does wake it.
    send(
        &state,
        post("/api/serve/knob", serde_json::json!({"key": "memory_ratio", "value": "0.8"})),
    )
    .await;
    assert!(*state.changed.borrow() > before);
}

// ---------------------------------------------------------------- job output

/// `{id}` is a `u64`. A path that is not one used to be rejected by axum's own extractor,
/// with a plain-text body — the single `/api` response a client could not parse.
#[tokio::test]
async fn a_non_numeric_job_id_is_the_json_envelope_not_plain_text() {
    let state = populated().await;
    for path in ["/api/jobs/abc/output", "/api/jobs/-1/output", "/api/jobs/1.5/output"] {
        let (status, text, content_type) = raw(&state, get(path)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(content_type.as_deref(), Some("application/json"), "{path}");
        let value: Value = serde_json::from_str(&text).expect("the envelope is JSON");
        assert!(value["error"].is_string(), "{path}: {text}");
        assert_eq!(value["toasted"], false);
    }
}

/// Every line comes back classified, so the browser colors a log exactly as the terminal
/// does rather than reimplementing the rule.
#[tokio::test]
async fn engine_log_lines_carry_the_severity_the_terminal_colors_them_with() {
    let state = unguarded().await;
    state.write(|app| {
        for line in [
            "[ft-man] $ ft serve --model x",
            "INFO: loading weights",
            "WARNING: falling back to torch",
            "ERROR:freetoken.engine:boom",
        ] {
            app.engine.log.push(line.into(), false);
        }
    });

    let (status, body) = send(&state, get("/api/logs")).await;
    assert_eq!(status, StatusCode::OK);
    let severities: Vec<&str> =
        body["items"].as_array().unwrap().iter().map(|l| l["severity"].as_str().unwrap()).collect();
    assert_eq!(severities, vec!["meta", "normal", "warn", "error"]);
    assert!(body["items"][0]["text"].is_string(), "the text is unchanged beside it");
}

// ---------------------------------------------------------------- the heartbeat

/// Section 2.1: a heartbeat once a second when nothing changed, so a proxy between here
/// and the browser does not decide an idle connection is a dead one. It never fired,
/// because the ticker woke the stream five times a second whether or not anything moved.
#[tokio::test]
async fn an_idle_daemon_still_sends_a_heartbeat() {
    let state = unguarded().await;
    super::events::spawn_broadcaster(state.clone());

    let response = super::router(state.clone()).oneshot(get("/api/events")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let mut body = response.into_body().into_data_stream();
    let mut text = String::new();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while !text.contains("event: heartbeat") {
        match tokio::time::timeout_at(deadline, futures_util::StreamExt::next(&mut body)).await {
            Ok(Some(Ok(chunk))) => text.push_str(&String::from_utf8_lossy(&chunk)),
            _ => break,
        }
    }
    assert!(text.contains("event: snapshot"), "the stream opens with state: {text:.200}");
    assert!(
        text.contains("event: heartbeat"),
        "an idle daemon must prove the connection is alive: {text:.400}"
    );
}

/// A tick that moved nothing must not publish. Five identical documents a second to every
/// open browser is what the heartbeat was starved by.
#[tokio::test]
async fn an_idle_tick_publishes_nothing() {
    let state = unguarded().await;
    let before = *state.changed.borrow();
    for _ in 0..5 {
        state.write_if(|app| {
            let changed = app.tick();
            ((), changed)
        });
    }
    assert_eq!(*state.changed.borrow(), before, "an idle machine has nothing to say");

    // A toast is a change, and expiring it later is another one.
    state.write(|app| app.info("something happened"));
    assert!(*state.changed.borrow() > before);
}

// ---------------------------------------------------------------- the poll

/// Nothing is listening because nothing was started. That is the answer the port is
/// supposed to give, and the status field already says the engine is stopped, so the
/// snapshot carries no error for the Dashboard to color red.
#[tokio::test]
async fn a_refused_connection_with_no_engine_is_not_an_error() {
    let mut app = fresh().await;
    app.telemetry = crate::ui::app::Telemetry { unreachable: true, ..Default::default() };
    assert!(matches!(app.engine.state, crate::ft::EngineState::Stopped));

    assert_eq!(app.poll_error(), None, "a stopped engine's closed port is not a fault");
    assert!(!app.server_reachable());

    let state = WebState::new(app, Auth::new(None).unwrap());
    let (status, body) = send(&state, get("/api/snapshot")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["telemetry"]["error"].is_null(), "{}", body["telemetry"]);
}

/// The same refusal with an engine ft-man believes is running is worth saying out loud:
/// the two disagree, and only one of them can be right.
#[tokio::test]
async fn a_refused_connection_with_a_live_engine_is_reported() {
    let mut app = fresh().await;
    app.telemetry = crate::ui::app::Telemetry { unreachable: true, ..Default::default() };
    app.engine.state = crate::ft::EngineState::Running;

    let message = app.poll_error().expect("a live engine that will not answer is a fault");
    assert!(message.contains("nothing answers"), "{message}");
    assert!(message.contains(app.client.base_url()), "it names the endpoint: {message}");
}

/// A fault the server itself produced is reported whatever the engine is doing: it is not
/// the absence of an answer, it is a bad one.
#[tokio::test]
async fn a_server_side_failure_is_always_reported() {
    let mut app = fresh().await;
    app.telemetry = crate::ui::app::Telemetry {
        error: Some("/health returned 500".into()),
        ..Default::default()
    };
    assert_eq!(app.poll_error().as_deref(), Some("/health returned 500"));
}

/// The poll backs off while nothing is listening, so the moments that make an answer
/// likely have to say so. Starting an engine is the loudest of them: without this the
/// loading bar stays blank for as long as the backoff the closed port had earned.
#[tokio::test]
async fn starting_an_engine_wakes_the_backed_off_poll() {
    let app = fresh().await;
    let mut watcher = app.endpoint_tx.subscribe();
    assert!(!watcher.has_changed().unwrap(), "nothing pending before the wake");

    app.wake_poll();

    assert!(watcher.has_changed().unwrap(), "the poller is told to try again now");
    // And the endpoint itself is untouched: this is a nudge, not a reconfiguration.
    assert_eq!(*watcher.borrow_and_update(), *app.endpoint_tx.borrow());
}
