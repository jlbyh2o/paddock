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
    WebState::new(fresh().await, Auth::new(None))
}

async fn guarded() -> Shared {
    WebState::new(fresh().await, Auth::new(Some(TOKEN.into())))
}

async fn fresh() -> App {
    smoke::app().await
}

async fn populated() -> Shared {
    let mut app = fresh().await;
    smoke::populate(&mut app);
    WebState::new(app, Auth::new(None))
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

/// The fixture's jobs have a log path that was never written, which is the shape a daemon
/// restarted mid-conversion sees.
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
    let state = populated().await;
    let path = state.read(|app| app.models[0].path.display().to_string());

    let (status, body) =
        send(&state, post("/api/models/delete", serde_json::json!({"path": path}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "confirm_pending");

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
