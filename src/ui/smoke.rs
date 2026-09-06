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

fn app() -> App {
    crate::config::isolate_paths_for_tests();
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

    app.serve.set("model", "/models/Qwen3.6-35B-A3B-ftw");
    app.serve.set("moe_backend", "hybrid");
    app.serve.set("memory_ratio", "0.92");
    app.serve.set("enable_cache_report", "true");
}

#[tokio::test]
async fn every_view_renders_when_nothing_is_running() {
    let mut a = app();
    draw_all(&mut a);
}

#[tokio::test]
async fn every_view_renders_with_a_live_engine() {
    let mut a = app();
    populate(&mut a);
    draw_all(&mut a);
}

#[tokio::test]
async fn overlays_and_secondary_panes_render() {
    let mut a = app();
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
    a.hub_view.in_files = true;
    a.jobs_view.in_output = true;
    a.cache_view.set_pending(Pool::Moe, Some(1024));
    a.cache_view.set_pending(Pool::Kv, Some(65_536));
    draw_all(&mut a);

    // Every knob group, including the ones with the longest help text.
    for group in crate::knobs::Group::ALL {
        a.serve_view.group = group;
        a.serve_view.editing = true;
        draw_all(&mut a);
        a.serve_view.editing = false;
        draw_all(&mut a);
    }

    a.serve_view.naming = true;
    a.models_view.filtering = true;
    a.logs_view.filtering = true;
    a.hub_view.editing = true;
    draw_all(&mut a);
}

#[tokio::test]
async fn a_loading_engine_and_an_unreachable_server_both_render() {
    let mut a = app();

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
        let mut a = app();
        a.theme = crate::ui::theme::Theme::from_name(name);
        populate(&mut a);
        draw_all(&mut a);
    }
}

#[tokio::test]
async fn selection_stays_in_range_when_lists_shrink() {
    let mut a = app();
    populate(&mut a);

    // Point every cursor past the end, then redraw: the window logic must clamp rather
    // than index out of bounds.
    a.models_view.sel.index = 999;
    a.hub_view.sel.index = 999;
    a.hub_view.file_sel.index = 999;
    a.serve_view.sel.index = 999;
    a.serve_view.profile_sel.index = 999;
    a.cache_view.sel.index = 999;
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
        let mut a = app();
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
    let mut a = app();
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
    let mut a = app();
    a.engine.state = crate::ft::EngineState::Running;
    press_with(&mut a, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(a.should_quit);
    assert!(a.confirm.is_none());
}

#[tokio::test]
async fn quitting_with_a_managed_engine_asks_first() {
    let mut a = app();
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
    let mut a = app();
    press(&mut a, KeyCode::Char('q'));
    assert!(a.should_quit);
}

#[tokio::test]
async fn typing_in_the_model_filter_narrows_the_list() {
    let mut a = app();
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
    let mut a = app();
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
    let mut a = app();
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
    let mut a = app();
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
    let mut a = app();
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
    let mut a = app();
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
    let mut a = app();
    populate(&mut a);
    a.tab = Tab::Hub;
    a.hub_view.in_files = true;

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
    let mut a = app();
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
    let mut a = app();
    populate(&mut a);
    a.tab = Tab::Models;
    a.models_view.sel.index = 0; // the HF checkpoint, which has a converted sibling

    press(&mut a, KeyCode::Enter);
    assert_eq!(a.serve.get("model"), Some("/models/Qwen3.6-35B-A3B-ftw"));
    assert_eq!(a.tab, Tab::Serve);
}

#[tokio::test]
async fn log_scrolling_detaches_and_reattaches_the_tail() {
    let mut a = app();
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
    let mut a = app();
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
