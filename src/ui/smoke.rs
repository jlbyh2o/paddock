//! Render smoke tests.
//!
//! A panic inside `draw` corrupts the terminal and takes the whole tool down, and the
//! usual causes — a zero-width column, a slice index past the end of a short buffer, an
//! `Instant` subtraction — only appear at particular sizes or with particular data. So
//! every view is rendered across a range of terminal geometries, twice: once with the
//! empty state a fresh start shows, and once with every pane populated.

#![cfg(test)]

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::config::{Config, Profile, Profiles};
use crate::ft::proc::{JobKind, JobProgress, JobStatus};
use crate::ft::types::*;
use crate::hub::{Download, DownloadStatus, RepoInfo, RepoSummary, Sibling};
use crate::knobs::ServeConfig;
use crate::probe::{Gpu, Host};
use crate::ui::app::{App, Message, Pool, Tab, Telemetry};
use crate::ui::widgets::{Confirm, ConfirmAction, ToastKind};

/// Sizes worth covering: the smallest terminal anyone plausibly uses, an awkward narrow
/// one, a normal one, and a wide one. The narrow cases are where layout arithmetic
/// underflows.
const SIZES: &[(u16, u16)] = &[(40, 12), (60, 20), (80, 24), (120, 40), (200, 60)];

async fn app() -> App {
    crate::config::isolate_paths_for_tests();
    // App::new reads the serve state file to re-adopt a running engine, so it must not
    // race the supervision tests that write it.
    let _guard = crate::config::lock_serve_state().await;
    let (tx, _rx) = mpsc::unbounded_channel::<Message>();
    let mut config = Config::default();
    config.ui.theme = "dark".into();
    App::new(config, Profiles::default(), None, None, tx).expect("app should construct")
}

fn draw_all(app: &mut App) {
    for (w, h) in SIZES {
        let mut terminal = Terminal::new(TestBackend::new(*w, *h)).unwrap();
        for tab in Tab::ALL {
            app.tab = tab;
            terminal
                .draw(|f| super::draw::draw(f, app))
                .unwrap_or_else(|e| panic!("{tab:?} at {w}x{h}: {e}"));
        }
    }
}

/// Fill every pane with the kind of data a live system produces.
fn populate(app: &mut App) {
    app.telemetry = Telemetry {
        health: Some(Health {
            status: "ok".into(),
            model: Some("Qwen3.6-35B-A3B".into()),
            uptime_s: Some(4231),
            maintenance: Some("serving".into()),
            ..Default::default()
        }),
        stats: Some(Stats {
            model: ModelCard {
                id: Some("Qwen3.6-35B-A3B".into()),
                ctx: 262_144,
                attn: Some("hybrid_linear".into()),
                moe: true,
                sampling: Some(serde_json::json!({"temperature": 0.7, "top_p": 0.8, "top_k": 20})),
            },
            uptime_s: 4231,
            kv: Some(PagePool { used_pages: 41_233, total_pages: 131_072, page_size: 1 }),
            mamba: Some(SlotPool { used_slots: 3, total_slots: 12 }),
            swa: Some(PagePool { used_pages: 900, total_pages: 4096, page_size: 128 }),
            vram_bytes: 21_000_000_000,
            gpus: vec![GpuCard {
                index: Some(0),
                name: Some("NVIDIA GeForce RTX 5090".into()),
                uuid: Some("GPU-9e8d7c6b-5a49-4f13-8207-c1b0a4e6d3f5".into()),
                total_bytes: 34_359_738_368,
            }],
            throughput: Throughput { decode_tps: 48.7, prefill_tps: 3120.5 },
            requests: RequestStats {
                active: 2,
                completed: 1487,
                p95_ms: 8340,
                ttft_mean_ms: 412,
                prompt_tokens_total: 8_412_990,
                completion_tokens_total: 1_204_331,
            },
        }),
        cache: Some(CacheStatus {
            state: "serving".into(),
            last_rebuild: Some(serde_json::json!({"moe_cache_size": 512, "num_pages": 131072})),
            geometry: CacheGeometry {
                num_pages: 131_072,
                page_size: 1,
                moe_cache_size: 512,
                num_mamba_slots: 12,
                num_experts: 128,
                num_moe_layers: 48,
                moe_cache_policy: Some("lru".into()),
                unit_bytes: UnitBytes {
                    kv_per_token: 98_304,
                    moe_per_expert: 4_194_304,
                    mamba_per_slot: 1_048_576,
                    swa_per_token: 12_288,
                },
                swa_full_tokens_ratio: 0.2,
                swa_page_size: 128,
                num_swa_pages: 4096,
                cache_budget_bytes: 24_000_000_000,
                limits: Some(serde_json::json!({"moe": {"max": 2048}, "kv": {"max": 262144}})),
                reasoning: Some(Reasoning {
                    gears: vec!["off".into(), "low".into(), "high".into()],
                    default: Some("low".into()),
                }),
            },
        }),
        error: None,
        at: Some(std::time::Instant::now()),
    };

    app.gpus = vec![Gpu {
        index: 0,
        name: "NVIDIA GeForce RTX 5090".into(),
        uuid: "GPU-9e8d7c6b-5a49-4f13-8207-c1b0a4e6d3f5".into(),
        memory_total: 34_359_738_368,
        memory_used: 30_000_000_000,
        utilization: Some(96),
        temperature: Some(71),
        power_watts: Some(498.0),
        power_limit_watts: Some(575.0),
        pcie_link: Some("gen5 x16".into()),
    }];
    app.host = Host {
        cpu_percent: 62.5,
        cpu_cores: 32,
        physical_cores: 16,
        memory_total: 137_438_953_472,
        memory_used: 96_000_000_000,
        swap_total: 8_589_934_592,
        swap_used: 1_073_741_824,
        load_avg: (7.2, 6.1, 5.4),
        hostname: "inference-01".into(),
        kernel: "6.12.0-amd64".into(),
        uptime_s: 903_442,
    };

    for i in 0..40u64 {
        app.series.decode_tps.push(30 + i % 25);
        app.series.prefill_tps.push(2000 + i * 17);
        app.series.gpu_util.push(60 + i % 40);
        app.series.active.push(i % 5);
    }

    app.models = vec![
        crate::models::Model {
            name: "Qwen3.6-35B-A3B".into(),
            repo: None,
            variant: None,
            path: "/models/Qwen3.6-35B-A3B".into(),
            format: crate::models::Format::Hf,
            size_bytes: 70_000_000_000,
            arch: Some("Qwen3MoeForCausalLM".into()),
            model_type: Some("qwen3_moe".into()),
            is_moe: true,
            num_experts: Some(128),
            num_layers: Some(48),
            quant: Some("nvfp4".into()),
            max_position: Some(262_144),
            ftw_fingerprint: None,
            converted_to: Some("/models/Qwen3.6-35B-A3B-ftw".into()),
            modified: Some(std::time::SystemTime::now()),
        },
        crate::models::Model {
            name: "Qwen3.6-35B-A3B-ftw".into(),
            repo: None,
            variant: None,
            path: "/models/Qwen3.6-35B-A3B-ftw".into(),
            format: crate::models::Format::Ftw,
            size_bytes: 69_000_000_000,
            arch: Some("Qwen3MoeForCausalLM".into()),
            model_type: Some("qwen3_moe".into()),
            is_moe: true,
            num_experts: Some(128),
            num_layers: Some(48),
            quant: Some("nvfp4".into()),
            max_position: Some(262_144),
            ftw_fingerprint: Some("9f2c1ab4e7".into()),
            converted_to: None,
            modified: None,
        },
    ];

    // A ring shaped like a real agent session: one cold prefill, then cached turns.
    for (i, (prompt, ttft)) in
        [(69_000u64, 23_100u64), (69_800, 780), (70_400, 790), (71_100, 800)].iter().enumerate()
    {
        app.requests_view.entries.push_back(RequestRecord {
            ts: format!("2026-09-06T18:0{i}:00Z"),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            status: 200,
            model: Some("Qwen3.6-35B-A3B".into()),
            duration_ms: 12_000,
            ttft_ms: Some(*ttft),
            prompt_tokens: Some(*prompt),
            completion_tokens: Some(700),
            stream: Some(true),
            error: None,
        });
    }

    app.jobs = vec![
        crate::ft::Job::fake(
            JobKind::Convert,
            "Qwen3.6-35B-A3B → FTW",
            JobStatus::Running,
            JobProgress {
                phase: "experts".into(),
                done: 30_000_000_000,
                total: 68_000_000_000,
                bytes: true,
            },
        ),
        crate::ft::Job::fake(
            JobKind::Bench,
            "CPU vs PCIe bandwidth",
            JobStatus::Failed("with status 1".into()),
            JobProgress::default(),
        ),
    ];
    app.downloads = vec![Download::fake(
        "nvidia/GLM-5.2-NVFP4",
        120_000_000_000,
        44_000_000_000,
        DownloadStatus::Running,
    )];

    app.bench_profile = Some(BenchProfile {
        version: 4,
        timestamp: Some("2026-09-05T12:00:00+00:00".into()),
        host: Some("inference-01".into()),
        gpu: BenchGpu {
            index: Some(0),
            name: Some("NVIDIA GeForce RTX 5090".into()),
            uuid: Some("GPU-9e8d7c6b".into()),
        },
        cpu: BenchCpu { physical_cores: 32, threads_used: 32 },
        threshold: 2.0,
        ceilings: BenchCeilings {
            cpu_stream_read_gbs: 84.2,
            pcie_linear_h2d_gbs: 25.1,
            pcie_linear_d2h_gbs: 24.4,
        },
        dtypes: [("nvfp4".to_string(), Some("hybrid".to_string()))].into_iter().collect(),
        dtype_kernels: [(
            "nvfp4".to_string(),
            BenchKernel {
                cpu_moe_gbs: Some(61.3),
                cpu_moe_isa: Some("avx512_vnni".into()),
                pcie_gather_gbs: Some(21.8),
                cpu_moe_overlap_gbs: Some(48.0),
                pcie_gather_overlap_gbs: Some(18.2),
                ratio: Some(2.81),
                recommended: Some("hybrid".into()),
                note: Some("isa tier labels are nominal for this W4A8 kernel".into()),
            },
        )]
        .into_iter()
        .collect(),
    });

    app.hub_view.results = vec![RepoSummary {
        id: "Qwen/Qwen3.6-35B-A3B".into(),
        downloads: 1_204_331,
        likes: 4211,
        last_modified: Some("2026-08-01T10:00:00.000Z".into()),
        tags: vec!["moe".into(), "text-generation".into(), "license:apache-2.0".into()],
        gated: serde_json::Value::Bool(false),
        private: false,
    }];
    let siblings = vec![
        Sibling { path: "config.json".into(), size: Some(1400) },
        Sibling { path: "model-00001-of-00030.safetensors".into(), size: Some(4_000_000_000) },
        Sibling { path: "pytorch_model.bin".into(), size: Some(4_000_000_000) },
    ];
    app.hub_view.files = crate::hub::select_files(&siblings, &["*.bin".to_string()]);
    app.hub_view.info = Some(RepoInfo {
        id: "Qwen/Qwen3.6-35B-A3B".into(),
        sha: Some("a1b2c3d4e5f6a7b8".into()),
        gated: serde_json::Value::Bool(false),
        siblings,
    });
    app.supported_archs = Some(vec!["Qwen3MoeForCausalLM".into()]);
    app.hub_view.compat = Some(crate::compat::evaluate(
        &serde_json::json!({
            "architectures": ["Qwen3MoeForCausalLM"],
            "num_experts": 128,
            "max_position_embeddings": 262144,
            "quantization_config": {"format": "nvfp4-pack-quantized"}
        }),
        20 << 30,
        app.supported_archs.as_deref(),
        crate::compat::Hardware {
            vram_bytes: 16 << 30,
            host_ram_bytes: 40 << 30,
            free_disk_bytes: 60 << 30,
        },
    ));

    for i in 0..30u64 {
        app.requests_view.entries.push_back(RequestRecord {
            ts: "2026-09-05T14:23:07.123456Z".into(),
            method: "POST".into(),
            path: "/v1/chat/completions".into(),
            status: if i % 7 == 0 { 500 } else { 200 },
            model: Some("Qwen3.6-35B-A3B".into()),
            duration_ms: 1200 + i * 40,
            ttft_ms: Some(300 + i),
            prompt_tokens: Some(4000 + i * 10),
            completion_tokens: Some(200 + i),
            stream: Some(true),
            error: (i % 7 == 0).then(|| "upstream aborted".to_string()),
        });
    }

    for i in 0..50 {
        app.engine.log.push(format!("INFO:freetoken.engine:step {i} decode 48.7 tok/s"), false);
    }
    app.engine.log.push("ERROR:freetoken.engine:Traceback (most recent call last):".into(), true);

    app.profiles.items = vec![Profile {
        name: "qwen-hybrid".into(),
        notes: String::new(),
        serve: {
            let mut c = ServeConfig::new();
            c.set("model", "/models/Qwen3.6-35B-A3B-ftw");
            c.set("moe_backend", "hybrid");
            c
        },
    }];
    app.profiles.last_used = Some("qwen-hybrid".into());

    app.templates_view.stored = vec![crate::templates::StoredTemplate {
        name: "Qwen-Sharp-Chat-Templates".into(),
        path: "/state/templates/Qwen-Sharp-Chat-Templates.jinja".into(),
        meta: crate::templates::TemplateMeta {
            source: Some("peculiar-ragdoll/Qwen-Sharp-Chat-Templates".into()),
            revision: Some("5cb86e230acb03ffd992b841ecb12318a518e374".into()),
            repo_path: Some("chat_template.jinja".into()),
            fetched_at: Some("2026-09-05T21:00:00+00:00".into()),
            version: Some("qwen3.8-froggeric-v22.4.1".into()),
        },
        size: 29_686,
    }];
    app.templates_view.remote = vec![
        crate::hub::Sibling { path: "chat_template.jinja".into(), size: Some(29_686) },
        crate::hub::Sibling {
            path: "archive/v22.3.2-sharp/chat_template.jinja".into(),
            size: Some(28_577),
        },
    ];
    app.templates_view.remote_repo = Some("peculiar-ragdoll/Qwen-Sharp-Chat-Templates".into());
    app.templates_view.remote_revision = Some("5cb86e230acb".into());
    app.templates_view.repo.set("peculiar-ragdoll/Qwen-Sharp-Chat-Templates");
    app.templates_view.preview = Some((
        "Qwen-Sharp-Chat-Templates".into(),
        "{%- set template_version = \"qwen3.8-froggeric-v22.4.1\" %}\n{{ messages }}".into(),
    ));

    app.serve.set("model", "/models/Qwen3.6-35B-A3B-ftw");
    app.serve.set("moe_backend", "hybrid");
    app.serve.set("memory_ratio", "0.92");
    app.serve.set("enable_cache_report", "true");
}

#[tokio::test]
async fn every_view_renders_when_nothing_is_running() {
    let mut a = app().await;
    draw_all(&mut a);
}

#[tokio::test]
async fn every_view_renders_with_a_live_engine() {
    let mut a = app().await;
    populate(&mut a);
    draw_all(&mut a);
}

/// The whole point of the planner, end to end: a live engine that is quietly serving a
/// fraction of the model's context is noticed, explained, and fixed by one keystroke.
#[tokio::test]
async fn a_starved_context_is_detected_and_the_plan_repairs_it() {
    let mut a = app().await;
    populate(&mut a);
    a.serve.set("model", "/models/Qwen3.6-35B-A3B");

    // The fixture engine holds 131,072 KV tokens against a checkpoint offering 262,144.
    let fit = a.context_fit().expect("a serving engine reports both numbers");
    assert!(fit.is_truncated(), "half the advertised context is missing");
    assert_eq!(fit.summary(), "128k of 256k");

    let plan = crate::ui::views::plan::build(&a).expect("a plan should build");
    assert!(plan.unpriced.is_none(), "a live engine prices the split exactly");

    // This card cannot actually hold 256k of this model's KV, so the plan reaches for the
    // most it can hold rather than promising the ceiling — and says the ceiling is out of
    // reach rather than quietly settling.
    let costs = a.costs_for("Qwen3.6-35B-A3B").expect("the live engine priced it");
    let reachable = costs.max_context(false);
    assert!(reachable > fit.usable, "the plan should still win back a lot of context");
    assert!(reachable < fit.ceiling, "but not all of it, on this budget");
    assert_eq!(plan.fit.unwrap().usable, reachable);
    assert!(plan
        .steps
        .iter()
        .any(|s| s.level == crate::plan::Level::Warning && s.reason.contains("cannot hold")));

    // It pays for that context out of the expert cache and the prefill overlap's buffers.
    let reserve = plan
        .edits()
        .into_iter()
        .find(|(k, _)| *k == "kv_reserve_tokens")
        .expect("the plan should raise the KV reserve");
    assert_eq!(reserve.1, reachable.to_string());
    assert!(plan.edits().iter().any(|(k, _)| *k == "disable_moe_prefill_overlap"));

    let changed = plan.apply(&mut a.serve);
    assert!(changed > 0);
    assert_eq!(a.serve.get("kv_reserve_tokens"), Some(reachable.to_string().as_str()));

    // And the configuration it produced is one FreeToken will accept.
    assert!(a.serve.validate().is_empty(), "a planned configuration must be a valid one");
    assert!(a.serve.to_args().windows(2).any(|w| w[0] == "--kv-reserve-tokens"));
}

/// Costs are only knowable from a running engine, so they are remembered for the next
/// launch — otherwise every plan for a stopped engine would be unpriced forever.
#[tokio::test]
async fn measured_costs_outlive_the_engine_that_measured_them() {
    let mut a = app().await;
    populate(&mut a);
    a.serve.set("model", "/models/Qwen3.6-35B-A3B");

    // A poll of a ready engine records what it measured.
    let telemetry = std::mem::take(&mut a.telemetry);
    a.handle(Message::Telemetry(Box::new(telemetry)));
    assert!(
        a.cost_store.get("Qwen3.6-35B-A3B").is_some(),
        "a ready engine's unit costs should be written down"
    );

    // With the engine gone, the plan is still priced — from the store, not a guess.
    a.telemetry = Telemetry::default();
    assert!(a.context_fit().is_none(), "no engine, no live geometry");
    let plan = crate::ui::views::plan::build(&a).expect("a plan should build");
    assert!(plan.unpriced.is_none(), "the remembered costs still price it");
    assert!(plan.fit.is_some());
}

/// A model nothing has ever measured must produce advice, not invented arithmetic.
#[tokio::test]
async fn an_unmeasured_model_is_planned_without_inventing_a_cache_split() {
    let mut a = app().await;
    populate(&mut a);
    a.telemetry = Telemetry::default();
    a.serve.set("model", "/models/never-served");

    let plan = crate::ui::views::plan::build(&a).expect("a plan should still build");
    assert!(plan.unpriced.is_some(), "it should say why the split was skipped");
    assert!(plan.fit.is_none());
    assert!(
        !plan.edits().iter().any(|(k, _)| *k == "kv_reserve_tokens"),
        "no KV reserve may be recommended without a measured cost"
    );
}

/// The Dashboard's prefix-reuse figure is inferred, so the thing worth testing is that
/// it only appears when the evidence supports it.
#[tokio::test]
async fn prefix_reuse_is_estimated_from_the_ring_or_withheld() {
    let mut a = app().await;
    populate(&mut a);

    let r = a.prefix_reuse().expect("a cold request plus cached ones should estimate");
    assert!(r.fraction > 0.6, "the session is mostly cached, got {:.2}", r.fraction);
    assert!(r.summary().contains('~'), "it must read as an estimate: {}", r.summary());

    // Rebuild the ring with only the cached turns: the cold anchor is gone, the spread
    // goes with it, and so must the estimate.
    let cached: Vec<RequestRecord> = a
        .requests_view
        .entries
        .iter()
        .filter(|e| {
            e.ttft_ms.is_some_and(|t| t < 1000) && e.prompt_tokens.is_some_and(|p| p > 60_000)
        })
        .cloned()
        .collect();
    assert!(cached.len() >= 3, "fixture should hold several cached turns");
    a.requests_view.entries = cached.into_iter().collect();
    assert!(
        a.prefix_reuse().is_none(),
        "uniformly fast requests cannot be told apart from a fast GPU"
    );

    // And with no ring at all there is nothing to infer from.
    a.requests_view.entries.clear();
    assert!(a.prefix_reuse().is_none());
}

#[tokio::test]
async fn overlays_and_secondary_panes_render() {
    let mut a = app().await;
    populate(&mut a);

    a.show_help = true;
    draw_all(&mut a);
    a.show_help = false;

    a.confirm = Some(Confirm::new(
        "Stop the engine",
        vec![
            "Stop the engine serving Qwen3.6-35B-A3B?".into(),
            String::new(),
            "In-flight requests are aborted.".into(),
        ],
        ConfirmAction::StopEngine { force: false },
        false,
    ));
    draw_all(&mut a);
    a.confirm = None;

    a.toast("conversion finished", ToastKind::Success);
    a.toast("the engine exited with status 1 — see the Logs tab", ToastKind::Error);
    draw_all(&mut a);

    // Panes and modes that are hidden by default.
    a.serve_view.show_preview = true;
    a.serve_view.in_profiles = true;
    a.requests_view.show_details = true;
    a.logs_view.wrap = true;
    a.logs_view.errors_only = true;
    a.hub_view.focus = crate::ui::app::HubFocus::Files;
    a.jobs_view.in_output = true;
    a.templates_view.pane = crate::ui::app::TemplatePane::Remote;
    a.templates_view.preflight = Some((
        "Qwen-Sharp-Chat-Templates".into(),
        crate::ft::Preflight::Ok("4210 chars, 980 tokens (tool calls)".into()),
    ));
    a.cache_view.set_pending(Pool::Moe, Some(1024));
    a.cache_view.set_pending(Pool::Kv, Some(65_536));
    draw_all(&mut a);

    a.serve.set("model", "/models/Qwen3.6-35B-A3B");
    a.serve_view.plan = Some(crate::ui::views::plan::build(&a).expect("a plan should build"));
    draw_all(&mut a);
    a.serve_view.plan = None;

    // Every knob group, including the ones with the longest help text.
    for group in crate::knobs::Group::ALL {
        a.serve_view.group = group;
        a.serve_view.editing = true;
        draw_all(&mut a);
        a.serve_view.editing = false;
        draw_all(&mut a);
    }

    // A failed render check must render as legibly as a passing one.
    a.templates_view.preflight = Some((
        "Qwen-Sharp-Chat-Templates".into(),
        crate::ft::Preflight::Fail(
            "UndefinedError: 'dict object' has no attribute 'reasoning_content'".into(),
        ),
    ));
    draw_all(&mut a);
    a.templates_view.preflight = Some((
        "Qwen-Sharp-Chat-Templates".into(),
        crate::ft::Preflight::Warn(
            "83 chars, 21 tokens (tools listed); the tool-call form did not render".into(),
        ),
    ));
    a.templates_view.checking = true;
    draw_all(&mut a);
    a.templates_view.checking = false;

    a.templates_view.editing_repo = true;
    a.serve_view.naming = true;
    a.models_view.filtering = true;
    a.logs_view.filtering = true;
    a.hub_view.editing = true;
    draw_all(&mut a);
}

#[tokio::test]
async fn a_loading_engine_and_an_unreachable_server_both_render() {
    let mut a = app().await;

    a.telemetry.health = Some(Health {
        status: "loading".into(),
        model: Some("DeepSeek-V4-Flash".into()),
        phase: Some("weights".into()),
        progress: Some(LoadProgress { done_bytes: 40_000_000_000, total_bytes: 0 }),
        ..Default::default()
    });
    draw_all(&mut a);

    a.telemetry.health.as_mut().unwrap().progress =
        Some(LoadProgress { done_bytes: 40_000_000_000, total_bytes: 200_000_000_000 });
    draw_all(&mut a);

    a.telemetry = Telemetry {
        error: Some("GET /health: connection refused".into()),
        at: Some(std::time::Instant::now()),
        ..Default::default()
    };
    draw_all(&mut a);

    a.telemetry.health = Some(Health {
        status: "error".into(),
        message: Some("CUDA out of memory while allocating the KV pool".into()),
        ..Default::default()
    });
    draw_all(&mut a);
}

#[tokio::test]
async fn each_theme_renders() {
    for name in ["dark", "light", "mono", "auto"] {
        let mut a = app().await;
        a.theme = crate::ui::theme::Theme::from_name(name);
        populate(&mut a);
        draw_all(&mut a);
    }
}

#[tokio::test]
async fn selection_stays_in_range_when_lists_shrink() {
    let mut a = app().await;
    populate(&mut a);

    // Point every cursor past the end, then redraw: the window logic must clamp rather
    // than index out of bounds.
    a.models_view.sel.index = 999;
    a.hub_view.sel.index = 999;
    a.hub_view.file_sel.index = 999;
    a.serve_view.sel.index = 999;
    a.serve_view.profile_sel.index = 999;
    a.cache_view.sel.index = 999;
    a.templates_view.sel.index = 999;
    a.templates_view.remote_sel.index = 999;
    a.jobs_view.sel.index = 999;
    a.requests_view.sel.index = 999;
    draw_all(&mut a);

    // Then empty everything out underneath them.
    a.models.clear();
    a.hub_view.results.clear();
    a.hub_view.files.clear();
    a.hub_view.info = None;
    a.jobs.clear();
    a.downloads.clear();
    a.requests_view.entries.clear();
    a.profiles.items.clear();
    a.templates_view.stored.clear();
    a.templates_view.remote.clear();
    a.templates_view.preview = None;
    draw_all(&mut a);
}

// ---------------------------------------------------------------- input

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn press(app: &mut App, code: KeyCode) {
    super::input::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn press_with(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    super::input::handle_key(app, KeyEvent::new(code, mods));
}

/// Every printable key and every navigation key, on every tab, in both the empty and
/// populated states — then a redraw, so a key that corrupts state is caught here rather
/// than by a user.
#[tokio::test]
async fn no_key_on_any_tab_panics() {
    for populated in [false, true] {
        let mut a = app().await;
        if populated {
            populate(&mut a);
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();

        for tab in Tab::ALL {
            for code in navigation_keys().into_iter().chain(printable_keys()) {
                a.tab = tab;
                // Never let a confirmation swallow the rest of the sweep.
                a.confirm = None;
                a.show_help = false;
                press(&mut a, code);
                a.should_quit = false;
                terminal
                    .draw(|f| super::draw::draw(f, &mut a))
                    .unwrap_or_else(|e| panic!("{tab:?} after {code:?}: {e}"));
            }
        }
    }
}

fn navigation_keys() -> Vec<KeyCode> {
    vec![
        KeyCode::Up,
        KeyCode::Down,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::PageUp,
        KeyCode::PageDown,
        KeyCode::Home,
        KeyCode::End,
        KeyCode::Enter,
        KeyCode::Esc,
        KeyCode::Tab,
        KeyCode::BackTab,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::F(1),
    ]
}

fn printable_keys() -> Vec<KeyCode> {
    ("abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/? ")
        .chars()
        .map(KeyCode::Char)
        .collect()
}

#[tokio::test]
async fn digits_and_tab_switch_views() {
    let mut a = app().await;
    press(&mut a, KeyCode::Char('3'));
    assert_eq!(a.tab, Tab::Hub);
    // Tab is local to the Hub view, so it must not move to the next tab there.
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.tab, Tab::Hub);

    press(&mut a, KeyCode::Char('1'));
    assert_eq!(a.tab, Tab::Dashboard);
    press(&mut a, KeyCode::Tab);
    assert_eq!(a.tab, Tab::Models);
    press(&mut a, KeyCode::BackTab);
    assert_eq!(a.tab, Tab::Dashboard);
}

#[tokio::test]
async fn ctrl_c_quits_immediately_even_with_an_engine_running() {
    let mut a = app().await;
    a.engine.state = crate::ft::EngineState::Running;
    press_with(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(a.should_quit);
    assert!(a.confirm.is_none());
}

#[tokio::test]
async fn quitting_with_a_managed_engine_asks_first() {
    let mut a = app().await;
    a.engine.state = crate::ft::EngineState::Running;
    press(&mut a, KeyCode::Char('q'));
    assert!(!a.should_quit, "q must not quit outright while an engine is up");
    assert!(a.confirm.is_some());

    // The safe option is preselected, so a bare Enter dismisses without quitting.
    press(&mut a, KeyCode::Enter);
    assert!(!a.should_quit);
    assert!(a.confirm.is_none());

    press(&mut a, KeyCode::Char('q'));
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);
    assert!(a.should_quit);
}

#[tokio::test]
async fn quitting_with_no_engine_needs_no_confirmation() {
    let mut a = app().await;
    press(&mut a, KeyCode::Char('q'));
    assert!(a.should_quit);
}

#[tokio::test]
async fn typing_in_the_model_filter_narrows_the_list() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Models;
    assert_eq!(a.filtered_models().len(), 2);

    press(&mut a, KeyCode::Char('/'));
    assert!(a.models_view.filtering);
    for c in "ftw".chars() {
        press(&mut a, KeyCode::Char(c));
    }
    assert_eq!(a.models_view.filter.value, "ftw");
    assert_eq!(a.filtered_models().len(), 1);

    // Esc abandons the filter; Enter would have kept it.
    press(&mut a, KeyCode::Esc);
    assert!(!a.models_view.filtering);
    assert_eq!(a.filtered_models().len(), 2);
}

#[tokio::test]
async fn editing_a_knob_validates_before_it_commits() {
    let mut a = app().await;
    a.tab = Tab::Serve;
    a.serve_view.group = crate::knobs::Group::Memory;
    // memory_ratio is the first knob in that group.
    a.serve_view.sel.index = 0;

    press(&mut a, KeyCode::Enter);
    assert!(a.serve_view.editing);
    for c in "1.5".chars() {
        press(&mut a, KeyCode::Char(c));
    }
    press(&mut a, KeyCode::Enter);
    assert!(!a.serve.is_set("memory_ratio"), "an out-of-range value must be rejected");

    press(&mut a, KeyCode::Enter);
    for c in "0.85".chars() {
        press(&mut a, KeyCode::Char(c));
    }
    press(&mut a, KeyCode::Enter);
    assert_eq!(a.serve.get("memory_ratio"), Some("0.85"));

    // x clears it back to the engine default.
    press(&mut a, KeyCode::Char('x'));
    assert!(!a.serve.is_set("memory_ratio"));
}

#[tokio::test]
async fn cycling_a_choice_knob_wraps_through_unset() {
    let mut a = app().await;
    a.tab = Tab::Serve;
    a.serve_view.group = crate::knobs::Group::Moe;
    a.serve_view.sel.index = 0; // moe_backend

    let options = ["auto", "offload", "hybrid", "cpu", "fused"];
    for expected in options {
        press(&mut a, KeyCode::Char(' '));
        assert_eq!(a.serve.get("moe_backend"), Some(expected));
    }
    press(&mut a, KeyCode::Char(' '));
    assert!(!a.serve.is_set("moe_backend"), "cycling past the end returns to the default");
}

#[tokio::test]
async fn a_flag_knob_toggles_on_enter() {
    let mut a = app().await;
    a.tab = Tab::Serve;
    a.serve_view.group = crate::knobs::Group::Api;
    let idx = crate::knobs::knobs_in(crate::knobs::Group::Api)
        .position(|k| k.key == "enable_cache_report")
        .unwrap();
    a.serve_view.sel.index = idx;

    press(&mut a, KeyCode::Enter);
    assert!(a.serve.flag("enable_cache_report"));
    assert!(a.serve.to_args().contains(&"--enable-cache-report".to_string()));
    press(&mut a, KeyCode::Enter);
    assert!(!a.serve.flag("enable_cache_report"));
}

#[tokio::test]
async fn adjusting_a_cache_pool_stages_a_change_and_resets_cleanly() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Cache;
    a.cache_view.sel.index = 0; // MoE

    press(&mut a, KeyCode::Right);
    let staged = a.cache_view.pending_for(Pool::Moe).expect("an edit should be staged");
    assert!(staged > 512, "right should increase the pool");

    press(&mut a, KeyCode::Char('r'));
    assert_eq!(a.cache_view.pending_for(Pool::Moe), None);

    // Shift moves in bigger steps than a bare arrow.
    press_with(&mut a, KeyCode::Right, KeyModifiers::SHIFT);
    let big = a.cache_view.pending_for(Pool::Moe).unwrap();
    press(&mut a, KeyCode::Char('R'));
    press(&mut a, KeyCode::Right);
    let small = a.cache_view.pending_for(Pool::Moe).unwrap();
    assert!(big > small, "Shift+Right ({big}) should exceed Right ({small})");
}

#[tokio::test]
async fn applying_a_cache_rebuild_asks_first() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Cache;
    a.cache_view.set_pending(Pool::Kv, Some(65_536));

    press(&mut a, KeyCode::Char('a'));
    let confirm = a.confirm.as_ref().expect("a rebuild should be confirmed");
    assert_eq!(confirm.action, ConfirmAction::ApplyCacheRebuild);
    assert!(confirm.body.iter().any(|l| l.contains("KV pages")));
}

#[tokio::test]
async fn toggling_hub_files_updates_the_selection() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Hub;
    a.hub_view.focus = crate::ui::app::HubFocus::Files;

    let before = a.hub_view.files.iter().filter(|f| f.wanted).count();
    press(&mut a, KeyCode::Char('n'));
    assert_eq!(a.hub_view.files.iter().filter(|f| f.wanted).count(), 0);
    press(&mut a, KeyCode::Char('a'));
    assert_eq!(a.hub_view.files.iter().filter(|f| f.wanted).count(), a.hub_view.files.len());

    press(&mut a, KeyCode::Char('n'));
    a.hub_view.file_sel.index = 0;
    press(&mut a, KeyCode::Char(' '));
    assert!(a.hub_view.files[0].wanted);
    assert!(before > 0);
}

#[tokio::test]
async fn deleting_a_model_is_confirmed_and_names_the_path() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Models;
    a.models_view.sel.index = 0;

    press(&mut a, KeyCode::Char('D'));
    let confirm = a.confirm.as_ref().expect("deletion must be confirmed");
    assert!(confirm.destructive);
    assert!(!confirm.accepted(), "the safe option must be preselected");
    assert!(confirm.body.iter().any(|l| l.contains("/models/Qwen3.6-35B-A3B")));
}

#[tokio::test]
async fn selecting_a_model_prefers_its_ftw_build() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Models;
    a.models_view.sel.index = 0; // the HF checkpoint, which has a converted sibling

    press(&mut a, KeyCode::Enter);
    assert_eq!(a.serve.get("model"), Some("/models/Qwen3.6-35B-A3B-ftw"));
    assert_eq!(a.tab, Tab::Serve);
}

#[tokio::test]
async fn log_scrolling_detaches_and_reattaches_the_tail() {
    let mut a = app().await;
    populate(&mut a);
    a.tab = Tab::Logs;
    assert!(a.logs_view.follow);

    press(&mut a, KeyCode::Up);
    assert!(!a.logs_view.follow);
    assert_eq!(a.logs_view.scroll, 1);

    press(&mut a, KeyCode::Char('G'));
    assert!(a.logs_view.follow);
    assert_eq!(a.logs_view.scroll, 0);
}

#[tokio::test]
async fn saving_and_loading_a_profile_round_trips_the_configuration() {
    let mut a = app().await;
    a.tab = Tab::Serve;
    a.serve.set("model", "/models/test");
    a.serve.set("moe_backend", "cpu");

    press(&mut a, KeyCode::Char('S'));
    assert!(a.serve_view.naming);
    press(&mut a, KeyCode::Char('X'));
    press(&mut a, KeyCode::Enter);

    let saved = a.profiles.items.iter().find(|p| p.name.ends_with('X'));
    let saved = saved.expect("the profile should be saved");
    assert_eq!(saved.serve.get("moe_backend"), Some("cpu"));

    a.serve = ServeConfig::new();
    a.serve_view.profile_sel.index =
        a.profiles.items.iter().position(|p| p.name.ends_with('X')).unwrap();
    press(&mut a, KeyCode::Char('P'));
    assert_eq!(a.serve.get("moe_backend"), Some("cpu"));
    assert_eq!(a.serve.get("model"), Some("/models/test"));
}

/// Render one tab and return everything visible, as text. Lets a test assert on what a
/// user would actually read, which is the only way to catch a view consulting the wrong
/// state — the data can be perfectly correct and still never reach the screen.
fn render_text(app: &mut App, tab: Tab, w: u16, h: u16) -> String {
    app.tab = tab;
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal.draw(|f| super::draw::draw(f, app)).unwrap();
    let buf = terminal.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Regression: the Hub view built a fresh `Config::default()` to decide whether a token
/// existed, so a token set in the user's config file was never seen and the view claimed
/// there was none — while the download client, reading the real config, was authenticated.
#[tokio::test]
async fn the_hub_view_reports_the_token_the_app_actually_holds() {
    let mut a = app().await;
    // Set this explicitly: the developer's machine may well have a real token cached at
    // ~/.cache/huggingface/token, and a test must not depend on whether it does.
    a.hub_token = None;

    let screen = render_text(&mut a, Tab::Hub, 110, 30);
    assert!(
        screen.contains("No Hugging Face token found"),
        "with no token the view should say so:\n{screen}"
    );

    a.hub_token = Some(crate::config::HubToken {
        value: "hf_secret".into(),
        source: "hub.token in the config",
    });
    let screen = render_text(&mut a, Tab::Hub, 110, 30);
    assert!(
        !screen.contains("No Hugging Face token found"),
        "a configured token must not be reported as missing:\n{screen}"
    );
    assert!(
        screen.contains("hub.token in the config"),
        "the view should name where the token came from:\n{screen}"
    );
    // And the secret itself never reaches the screen.
    assert!(!screen.contains("hf_secret"), "the token value must not be displayed");
}

/// The client that performs downloads must use the same token the view reports, or the
/// two can disagree in either direction.
#[tokio::test]
async fn the_download_client_uses_the_apps_token() {
    let mut a = app().await;
    a.hub_token = None;
    assert!(!super::input::hub_client_for_tests(&a).unwrap().has_token());

    a.hub_token = Some(crate::config::HubToken {
        value: "hf_secret".into(),
        source: "hub.token in the config",
    });
    assert!(super::input::hub_client_for_tests(&a).unwrap().has_token());
}

// ---------------------------------------------------------------- templates

/// Build a scratch checkpoint on disk and point the app's library at it, so the template
/// actions operate on a real directory rather than a fabricated path.
fn with_checkpoint(a: &mut App, tag: &str, with_own_template: bool) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ft-man-tpl-ui-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"model_type":"qwen3_moe"}"#).unwrap();
    std::fs::write(dir.join("model.safetensors"), b"x").unwrap();
    if with_own_template {
        std::fs::write(dir.join(crate::templates::TEMPLATE_FILE), "ORIGINAL {{ x }}").unwrap();
    }
    a.models = vec![crate::models::inspect(&dir).expect("scratch checkpoint should be recognized")];
    a.models_view.sel.index = 0;
    dir
}

/// Put one template into the real store the app reads, and select it.
fn with_stored_template(a: &mut App, name: &str, jinja: &str) {
    crate::templates::save(name, jinja, crate::templates::TemplateMeta::default()).unwrap();
    a.reload_templates();
    a.templates_view.sel.index =
        a.templates_view.stored.iter().position(|t| t.name == name).unwrap();
}

const SHARP: &str = "{%- set template_version = \"qwen3.8-froggeric-v22.4.1\" %}\n{{ messages }}";

#[tokio::test]
async fn applying_a_template_asks_first_and_names_every_directory_it_writes() {
    let mut a = app().await;
    a.config.templates.preflight = false;
    let dir = with_checkpoint(&mut a, "ask", true);
    with_stored_template(&mut a, "sharp-ask", SHARP);
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('a'));
    let confirm = a.confirm.as_ref().expect("applying must be confirmed");
    let body = confirm.body.join("\n");
    assert!(body.contains("chat_template.jinja"), "it should say what it writes:\n{body}");
    assert!(body.contains(&dir.display().to_string()), "it should name the directory:\n{body}");
    assert!(!confirm.accepted(), "the safe option must be preselected");

    // Declining changes nothing on disk.
    press(&mut a, KeyCode::Esc);
    assert!(matches!(crate::templates::status(&dir), crate::templates::Status::Foreign));
    assert_eq!(
        std::fs::read_to_string(dir.join(crate::templates::TEMPLATE_FILE)).unwrap(),
        "ORIGINAL {{ x }}"
    );

    std::fs::remove_dir_all(&dir).ok();
    crate::templates::remove("sharp-ask").ok();
}

#[tokio::test]
async fn accepting_applies_the_template_and_u_puts_the_original_back() {
    let mut a = app().await;
    a.config.templates.preflight = false;
    let dir = with_checkpoint(&mut a, "roundtrip", true);
    with_stored_template(&mut a, "sharp-roundtrip", SHARP);
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('a'));
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);

    assert_eq!(std::fs::read_to_string(dir.join(crate::templates::TEMPLATE_FILE)).unwrap(), SHARP);
    let status = crate::templates::status(&dir);
    assert!(status.is_overridden());
    assert!(status.label().contains("qwen3.8-froggeric-v22.4.1"), "{}", status.label());

    // And the Models tab reports the override rather than "built-in".
    let screen = render_text(&mut a, Tab::Models, 110, 30);
    assert!(screen.contains("Chat template"), "{screen}");
    assert!(screen.contains("sharp-roundtrip"), "the detail pane should name it:\n{screen}");

    a.tab = Tab::Templates;
    press(&mut a, KeyCode::Char('u'));
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);
    assert_eq!(
        std::fs::read_to_string(dir.join(crate::templates::TEMPLATE_FILE)).unwrap(),
        "ORIGINAL {{ x }}"
    );

    std::fs::remove_dir_all(&dir).ok();
    crate::templates::remove("sharp-roundtrip").ok();
}

#[tokio::test]
async fn a_template_is_applied_to_the_ftw_build_as_well() {
    let mut a = app().await;
    a.config.templates.preflight = false;
    let dir = with_checkpoint(&mut a, "ftw", false);
    let ftw = dir.with_file_name(format!("{}-ftw", dir.file_name().unwrap().to_string_lossy()));
    std::fs::create_dir_all(&ftw).unwrap();
    std::fs::write(ftw.join(crate::ft::proc::FTW_INDEX), r#"{"quant_format":"nvfp4"}"#).unwrap();
    a.models[0].converted_to = Some(ftw.clone());
    with_stored_template(&mut a, "sharp-ftw", SHARP);
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('a'));
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);

    // Serving either directory must get the same template.
    assert_eq!(std::fs::read_to_string(dir.join(crate::templates::TEMPLATE_FILE)).unwrap(), SHARP);
    assert_eq!(std::fs::read_to_string(ftw.join(crate::templates::TEMPLATE_FILE)).unwrap(), SHARP);

    press(&mut a, KeyCode::Char('u'));
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);
    assert!(!dir.join(crate::templates::TEMPLATE_FILE).exists());
    assert!(!ftw.join(crate::templates::TEMPLATE_FILE).exists());

    std::fs::remove_dir_all(&dir).ok();
    std::fs::remove_dir_all(&ftw).ok();
    crate::templates::remove("sharp-ftw").ok();
}

/// The version belongs in the bottom-right corner, and must yield to the hints rather than
/// overwrite them when the terminal is narrow.
#[tokio::test]
async fn the_version_sits_in_the_bottom_right_corner() {
    let mut a = app().await;
    let expected = concat!("v", env!("CARGO_PKG_VERSION"));

    let screen = render_text(&mut a, Tab::Dashboard, 120, 30);
    let last = screen.lines().last().expect("there is a footer").trim_end();
    assert!(last.ends_with(expected), "the last line should end with {expected}:\n{last:?}");

    // It is the corner, not merely somewhere on the line: nothing may follow it.
    assert_eq!(last.matches(expected).count(), 1, "{last:?}");

    // On a narrow terminal the hints matter more, so the version stands down instead of
    // being drawn over them.
    let narrow = render_text(&mut a, Tab::Serve, 40, 12);
    let last_narrow = narrow.lines().last().expect("there is a footer").trim_end();
    assert!(
        !last_narrow.contains(&format!("{expected}{expected}")),
        "it must never double-draw:\n{last_narrow:?}"
    );

    // And every tab keeps its hints legible, whatever the width.
    for tab in Tab::ALL {
        for (w, h) in [(40u16, 12u16), (80, 24), (200, 60)] {
            let _ = render_text(&mut a, tab, w, h);
        }
    }
}

/// Regression, reported as a hard crash: applying a template to a GGUF checkpoint that
/// lives in the Hugging Face cache panicked with "index out of bounds: the len is 0".
///
/// `targets` had been filtering cache paths out, so a cache-resident model with no FTW
/// build — which `ft checkpoint` cannot produce for a GGUF anyway — produced an empty
/// target list. The confirmation then named no directories, the apply reported success for
/// zero writes, and indexing that empty list took the whole TUI down.
#[tokio::test]
async fn applying_to_a_cache_resident_checkpoint_writes_it_and_does_not_panic() {
    let mut a = app().await;
    a.config.templates.preflight = false;

    // Shaped like a real cache entry, because that shape is what the bug turned on.
    let root = std::env::temp_dir().join(format!(
        "ft-man-cache-tpl-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let dir = root.join("models--acme--M-GGUF/snapshots/abc123/Q4_K_M");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("config.json"), r#"{"model_type":"qwen3_moe"}"#).unwrap();
    std::fs::write(dir.join("model-Q4_K_M.gguf"), b"x").unwrap();

    let mut model = crate::models::inspect(&dir).expect("the checkpoint should be recognized");
    model.name = "acme/M-GGUF:Q4_K_M".into();
    model.repo = Some("acme/M-GGUF".into());
    model.variant = Some("Q4_K_M".into());
    // No FTW build: a GGUF has none, which is exactly the case that crashed.
    model.converted_to = None;
    a.models = vec![model];
    a.models_view.sel.index = 0;

    with_stored_template(&mut a, "sharp-cache", SHARP);
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('a'));
    let confirm = a.confirm.as_ref().expect("applying must still be confirmed");
    let body = confirm.body.join("\n");
    assert!(body.contains(&dir.display().to_string()), "it must name a directory:\n{body}");
    // And say that this directory is not private to ft-man.
    assert!(
        body.contains("Hugging Face cache"),
        "the reader must be told other tools see this too:\n{body}"
    );

    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);

    assert_eq!(
        std::fs::read_to_string(dir.join(crate::templates::TEMPLATE_FILE)).unwrap(),
        SHARP,
        "the template must actually be written, not reported as applied to nothing"
    );
    assert!(
        a.toasts.iter().any(|t| t.text.contains("applied")),
        "and reported: {:?}",
        a.toasts.iter().map(|t| &t.text).collect::<Vec<_>>()
    );

    press(&mut a, KeyCode::Char('u'));
    press(&mut a, KeyCode::Right);
    press(&mut a, KeyCode::Enter);
    assert!(!dir.join(crate::templates::TEMPLATE_FILE).exists(), "u must undo it");

    std::fs::remove_dir_all(&root).ok();
    crate::templates::remove("sharp-cache").ok();
}

#[tokio::test]
async fn reverting_a_model_with_no_override_is_refused_not_silently_ignored() {
    let mut a = app().await;
    let dir = with_checkpoint(&mut a, "norevert", true);
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('u'));
    assert!(a.confirm.is_none(), "there is nothing to confirm");
    assert!(
        a.toasts.iter().any(|t| t.text.contains("not using an ft-man template override")),
        "the user should be told why nothing happened"
    );
    // The hand-placed template is untouched.
    assert_eq!(
        std::fs::read_to_string(dir.join(crate::templates::TEMPLATE_FILE)).unwrap(),
        "ORIGINAL {{ x }}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn applying_with_no_model_selected_explains_rather_than_failing() {
    let mut a = app().await;
    with_stored_template(&mut a, "sharp-nomodel", SHARP);
    a.models.clear();
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('a'));
    assert!(a.confirm.is_none());
    assert!(a.toasts.iter().any(|t| t.text.contains("no model selected")));

    crate::templates::remove("sharp-nomodel").ok();
}

#[tokio::test]
async fn the_templates_view_warns_that_a_running_engine_needs_a_restart() {
    let mut a = app().await;
    a.config.templates.preflight = false;
    let dir = with_checkpoint(&mut a, "restart", false);
    with_stored_template(&mut a, "sharp-restart", SHARP);
    a.engine.state = crate::ft::EngineState::Running;
    a.tab = Tab::Templates;

    press(&mut a, KeyCode::Char('a'));
    let body = a.confirm.as_ref().unwrap().body.join("\n");
    assert!(body.contains("restart"), "a loaded engine already read its template; say so:\n{body}");

    std::fs::remove_dir_all(&dir).ok();
    crate::templates::remove("sharp-restart").ok();
}

// ---------------------------------------------------------------- conversion

#[tokio::test]
async fn a_preflight_concern_asks_before_burning_minutes_on_a_conversion() {
    let mut a = app().await;
    let dir = with_checkpoint(&mut a, "cvtwarn", false);
    let source = a.models[0].path.clone();

    super::input::on_convert_preflight(
        &mut a,
        source.clone(),
        crate::ft::Preflight::Warn(
            "the checkpoint declares nvfp4-pack-quantized but FreeToken resolved its \
             experts as unquantized (expert_quant=none)"
                .into(),
        ),
    );

    let confirm = a.confirm.as_ref().expect("a doubtful conversion must be confirmed");
    let body = confirm.body.join("\n");
    assert!(body.contains("expert_quant=none"), "the reason must be quoted:\n{body}");
    assert!(body.contains("several minutes"), "say what it costs to proceed:\n{body}");
    assert!(!confirm.accepted(), "the safe option must be preselected");
    assert_eq!(confirm.action, ConfirmAction::ConvertAnyway(source));
    assert!(a.jobs.is_empty(), "nothing should have been spawned yet");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_failed_preflight_is_also_offered_rather_than_silently_blocking() {
    let mut a = app().await;
    let dir = with_checkpoint(&mut a, "cvtfail", false);
    let source = a.models[0].path.clone();

    // A check that could not run at all must not become an unexplained refusal: the
    // check is advisory, and the user may know better than it does.
    super::input::on_convert_preflight(
        &mut a,
        source.clone(),
        crate::ft::Preflight::Fail("ImportError: no module named torch".into()),
    );
    let confirm = a.confirm.as_ref().expect("a failed check should still offer the choice");
    assert!(confirm.body.join("\n").contains("ImportError"));

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn a_clean_preflight_starts_the_conversion_without_asking() {
    let mut a = app().await;
    let dir = with_checkpoint(&mut a, "cvtok", false);
    let source = a.models[0].path.clone();
    // No FreeToken CLI here, so the spawn fails — but the point is that nothing was
    // put in front of the user first.
    super::input::on_convert_preflight(
        &mut a,
        source,
        crate::ft::Preflight::Ok("Qwen3MoE: MoE, 128 experts x 48 layers".into()),
    );
    assert!(a.confirm.is_none(), "a clean check must not interrupt");

    std::fs::remove_dir_all(&dir).ok();
}

#[tokio::test]
async fn the_hub_reports_an_incompatible_repo_before_any_download() {
    let mut a = app().await;
    populate(&mut a);

    // An architecture FreeToken does not register: nothing to serve it with, and no
    // amount of flags changes that, so it is the blocker worth showing before a download.
    a.hub_view.compat = Some(crate::compat::evaluate(
        &serde_json::json!({
            "architectures": ["SomeNewThingForCausalLM"],
            "model_type": "some_new_thing",
            "text_config": {"num_experts": 256, "num_hidden_layers": 40}
        }),
        23 << 30,
        Some(&["Qwen3_5MoeForConditionalGeneration".to_string()]),
        crate::compat::Hardware {
            vram_bytes: 16 << 30,
            host_ram_bytes: 40 << 30,
            free_disk_bytes: 60 << 30,
        },
    ));

    let screen = render_text(&mut a, Tab::Hub, 130, 34);
    assert!(screen.contains("not supported"), "the verdict must be visible:\n{screen}");
    assert!(screen.contains("registry"), "and say why:\n{screen}");
    // The verdict must not run into the summary beside it.
    assert!(
        !screen.contains("not supportedSome"),
        "verdict and summary need a separator:\n{screen}"
    );
}

#[tokio::test]
async fn a_supported_repo_reads_as_supported() {
    let mut a = app().await;
    populate(&mut a);
    let screen = render_text(&mut a, Tab::Hub, 130, 34);
    assert!(screen.contains("supported"), "{screen}");
    assert!(!screen.contains("not supported"), "{screen}");
}

#[tokio::test]
async fn an_unreadable_registry_leaves_the_verdict_honest_rather_than_wrong() {
    let mut a = app().await;
    populate(&mut a);
    a.supported_archs = None;
    a.hub_view.compat = Some(crate::compat::evaluate(
        &serde_json::json!({"architectures": ["SomethingBrandNew"], "num_experts": 8}),
        1 << 30,
        None,
        crate::compat::Hardware::default(),
    ));
    let screen = render_text(&mut a, Tab::Hub, 130, 34);
    assert!(screen.contains("unverified"), "say it could not check:\n{screen}");
    assert!(!screen.contains("not in FreeToken"), "and do not claim it is unsupported");
}

/// Regression: one Enter fires two requests — the repo file listing and the
/// config.json compatibility check — and either can land first. `HubInfo` used to clear
/// the verdict unconditionally, so whenever the small config fetch won the race (which
/// is most of the time) the result was wiped the instant it arrived and the pane showed
/// nothing at all.
#[tokio::test]
async fn a_compatibility_verdict_survives_the_file_listing_landing_after_it() {
    for compat_first in [true, false] {
        let mut a = app().await;
        a.tab = Tab::Hub;

        let report = crate::compat::evaluate(
            &serde_json::json!({"architectures": ["Qwen3MoeForCausalLM"], "num_experts": 128}),
            1 << 30,
            Some(&["Qwen3MoeForCausalLM".to_string()]),
            crate::compat::Hardware::default(),
        );
        let info = Ok(RepoInfo {
            id: "Qwen/Qwen3.6-35B-A3B".into(),
            sha: Some("abc123".into()),
            gated: serde_json::Value::Bool(false),
            siblings: vec![crate::hub::Sibling { path: "config.json".into(), size: Some(1400) }],
        });

        if compat_first {
            a.handle(Message::Compatibility(Box::new(Ok(report))));
            a.handle(Message::HubInfo(Box::new(info)));
        } else {
            a.handle(Message::HubInfo(Box::new(info)));
            a.handle(Message::Compatibility(Box::new(Ok(report))));
        }

        assert!(
            a.hub_view.compat.is_some(),
            "the verdict must survive regardless of arrival order (compat_first={compat_first})"
        );
        let screen = render_text(&mut a, Tab::Hub, 130, 34);
        assert!(screen.contains("Compatibility"), "compat_first={compat_first}:\n{screen}");
    }
}

#[tokio::test]
async fn a_check_that_could_not_run_says_so_instead_of_looking_unchecked() {
    let mut a = app().await;
    a.tab = Tab::Hub;
    a.handle(Message::Compatibility(Box::new(Err("config.json: HTTP 404 Not Found".into()))));

    assert!(a.hub_view.compat.is_none());
    let screen = render_text(&mut a, Tab::Hub, 130, 34);
    assert!(screen.contains("could not check"), "the title should say so:\n{screen}");
    assert!(screen.contains("404"), "and quote the reason:\n{screen}");
}
