//! The Dashboard: engine state, throughput, cache pools and hardware at a glance.
//!
//! This is the screen someone leaves open on a second monitor, so it favors steady
//! layout over density — nothing here reflows as numbers change, and every meter keeps
//! its position whether or not the engine is up.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Sparkline, Wrap};
use ratatui::Frame;

use crate::ft::EngineState;
use crate::ui::app::App;
use crate::ui::widgets::meter_line;
use crate::util::{bytes, bytes_short, count, duration_secs};

pub fn render(f: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(11), Constraint::Length(9), Constraint::Min(6)])
        .split(cols[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(11), Constraint::Min(8), Constraint::Length(6)])
        .split(cols[1]);

    engine_pane(f, app, left[0]);
    throughput_pane(f, app, left[1]);
    cache_pane(f, app, left[2]);
    gpu_pane(f, app, right[0]);
    activity_pane(f, app, right[1]);
    host_pane(f, app, right[2]);
}

// ---------------------------------------------------------------- engine

fn engine_pane(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Engine", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut lines: Vec<Line> = Vec::new();

    let mut status = vec![Span::styled(format!("{:<18}", "Status"), t.label())];
    status.extend(t.status_dot(app.engine_status_color(), app.engine_status_text()));
    lines.push(Line::from(status));

    lines.push(t.field("Model", app.current_model().unwrap_or_else(|| "—".into())));

    if let Some(ref ver) = app.ft_version {
        lines.push(t.field("FreeToken", ver));
    }

    let endpoint = app.client.base_url().to_string();
    lines.push(t.field_colored(
        "Endpoint",
        endpoint,
        if app.server_reachable() { t.good } else { t.dim },
    ));

    match (&app.engine.state, app.engine.pid) {
        (EngineState::Adopted, Some(pid)) => {
            lines.push(t.field("Process", format!("pid {pid} (attached)")))
        }
        (_, Some(pid)) => lines.push(t.field("Process", format!("pid {pid}"))),
        (_, None) => lines.push(t.field("Process", "—")),
    }

    if let Some(h) = &app.telemetry.health {
        let uptime = app
            .telemetry
            .stats
            .as_ref()
            .map(|s| s.uptime_s)
            .filter(|s| *s > 0)
            .or_else(|| h.uptime_s.filter(|_| h.is_ready()));
        if let Some(up) = uptime {
            lines.push(t.field("Uptime", duration_secs(up)));
        }
        if h.is_loading() {
            let (done, total) =
                h.progress.as_ref().map(|p| (p.done_bytes, p.total_bytes)).unwrap_or((0, 0));
            let phase = h.phase.as_deref().unwrap_or("weights");
            let detail = if total > 0 {
                format!("{} / {}  ({phase})", bytes(done), bytes(total))
            } else {
                format!("{} so far  ({phase})", bytes(done))
            };
            lines.push(meter_line(t, "Loading", h.load_ratio().unwrap_or(0.0), 18, detail));
        }
    }

    if let Some(err) = &app.poll_error() {
        let age = app
            .telemetry
            .at
            .map(|at| format!(" ({} ago)", duration_secs(at.elapsed().as_secs())))
            .unwrap_or_default();
        lines.push(Line::from(Span::styled(
            crate::util::truncate(&format!("{err}{age}"), inner.width.saturating_sub(1) as usize),
            Style::default().fg(t.dim),
        )));
    }

    if let Some(sampling) = app
        .telemetry
        .stats
        .as_ref()
        .and_then(|s| s.model.sampling.as_ref())
        .and_then(format_sampling)
    {
        lines.push(t.field("Model sampling", sampling));
    }

    // The context the engine can actually serve, which is not the one the model card
    // advertises. `/v1/models` reports the checkpoint's ceiling on purpose, and the
    // engine quietly clamps to `num_pages * page_size` — so a checkpoint offering 256k
    // can be serving 8k with nothing anywhere saying so. This is the one place the two
    // numbers are put side by side.
    let fit = app.context_fit();
    if let Some(model) = app.telemetry.stats.as_ref().map(|s| &s.model) {
        let mut facts: Vec<String> = Vec::new();
        match fit {
            Some(fit) if fit.is_truncated() => facts.push(format!("{} ctx", fit.summary())),
            _ if model.ctx > 0 => facts.push(format!("{} ctx", crate::plan::tokens(model.ctx))),
            _ => {}
        }
        if let Some(a) = &model.attn {
            facts.push(a.clone());
        }
        if model.moe {
            facts.push("MoE".into());
        }
        // Anything past "text" is a tower this engine built and is holding VRAM for, so it
        // belongs on the line that says what is loaded.
        for modality in model.input_modalities.iter().filter(|m| *m != "text") {
            facts.push(modality.clone());
        }
        if !facts.is_empty() {
            let truncated = fit.is_some_and(|f| f.is_truncated());
            if truncated {
                lines.push(t.field_colored("Shape", facts.join(" · "), t.warn));
            } else {
                lines.push(t.field("Shape", facts.join(" · ")));
            }
        }
    }

    if let Some(fit) = fit.filter(|f| f.is_truncated()) {
        lines.push(Line::from(Span::styled(
            crate::util::truncate(
                &format!(
                    "KV holds {} of {} — press a on the Serve tab to plan a fix",
                    crate::plan::tokens(fit.usable),
                    crate::plan::tokens(fit.ceiling),
                ),
                inner.width.saturating_sub(1) as usize,
            ),
            Style::default().fg(t.warn),
        )));
    }

    // Git status of the FreeToken checkout this machine builds from.
    if let Some(c) = &app.ft_checkout {
        lines.push(Line::from(Span::styled(format!("Local checkout  {}", c.local_sha), t.label())));
        let width = (inner.width.saturating_sub(4)).max(20) as usize;
        lines.push(Line::from(Span::styled(
            format!("  {}", crate::util::truncate(&c.path, width)),
            t.muted(),
        )));
        let upstream = crate::util::truncate(&c.upstream, (inner.width - 18).max(20) as usize);
        lines.push(Line::from(Span::styled(format!("  {upstream}"), t.muted())));
        if c.upstream_sha.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (could not read upstream; is git installed?)",
                t.muted(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  upstream  {}", c.upstream_sha),
                t.muted(),
            )));
            if c.upstream_behind > 0 {
                lines.push(Line::from(Span::styled(
                    format!("  ⚠ {} commit(s) behind upstream", c.upstream_behind),
                    Style::default().fg(t.warn),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "  ✓ up to date with upstream",
                    Style::default().fg(t.good),
                )));
            }
            if c.origin_behind > 0 || c.origin_ahead > 0 {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  origin  {} ({} ahead, {} behind)",
                        c.origin_sha, c.origin_ahead, c.origin_behind
                    ),
                    t.muted(),
                )));
            }
            if c.dirty {
                lines.push(Line::from(Span::styled(
                    "  ⚠ working tree has local changes",
                    Style::default().fg(t.warn),
                )));
            }
            // Being on the right commit is not the same as running it: the kernels are
            // built, and a pull that touched their sources leaves the engine on the old
            // objects until someone rebuilds.
            if c.kernels_stale == Some(true) {
                lines.push(Line::from(Span::styled(
                    "  ⚠ kernels older than csrc/ — rebuild to run this commit",
                    Style::default().fg(t.warn),
                )));
            }
        }
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------- throughput

fn throughput_pane(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Throughput", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Min(0),
        ])
        .split(inner);

    let (decode, prefill) = app
        .telemetry
        .stats
        .as_ref()
        .map(|s| (s.throughput.decode_tps, s.throughput.prefill_tps))
        .unwrap_or((0.0, 0.0));

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{:<10}", "Decode"), t.label()),
            Span::styled(format!("{decode:>8.1}"), t.value()),
            Span::styled(" tok/s", t.muted()),
            Span::styled(format!("     peak {:.1}", app.series.decode_peak), t.muted()),
        ])),
        rows[0],
    );
    render_sparkline(f, app, rows[1], app.series.decode_tps.tail(rows[1].width as usize), t.accent);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{:<10}", "Prefill"), t.label()),
            Span::styled(format!("{prefill:>8.1}"), t.value()),
            Span::styled(" tok/s", t.muted()),
        ])),
        rows[2],
    );
    render_sparkline(f, app, rows[3], app.series.prefill_tps.tail(rows[3].width as usize), t.good);
}

fn render_sparkline(
    f: &mut Frame,
    app: &App,
    area: Rect,
    data: &[u64],
    color: ratatui::style::Color,
) {
    if area.height == 0 {
        return;
    }
    // A flat-zero series renders as an empty band rather than a misleading full bar.
    let max = data.iter().copied().max().unwrap_or(0).max(1);
    f.render_widget(
        Sparkline::default()
            .data(data)
            .max(max)
            .style(Style::default().fg(color))
            .absent_value_style(app.theme.muted()),
        area,
    );
}

// ---------------------------------------------------------------- cache

fn cache_pane(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Cache pools", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = (inner.width.saturating_sub(34)).clamp(8, 30) as usize;
    let mut lines: Vec<Line> = Vec::new();

    let stats = app.telemetry.stats.as_ref();
    let geo = app.telemetry.cache.as_ref().map(|c| &c.geometry);

    match stats.and_then(|s| s.kv.as_ref()) {
        Some(kv) if kv.total_pages > 0 => lines.push(meter_line(
            t,
            "KV",
            kv.ratio(),
            width,
            format!("{} / {} tok", count(kv.used_tokens()), count(kv.total_tokens())),
        )),
        _ => lines.push(idle_line(t, "KV", width)),
    }

    match geo.filter(|g| g.moe_cache_size > 0) {
        Some(g) => {
            let total = g.total_experts();
            let detail = if total > 0 {
                format!(
                    "{} / {} experts ({:.0}%)",
                    count(g.moe_cache_size),
                    count(total),
                    crate::util::ratio(g.moe_cache_size, total) * 100.0
                )
            } else {
                format!("{} slots", count(g.moe_cache_size))
            };
            lines.push(meter_line(
                t,
                "MoE",
                crate::util::ratio(g.moe_cache_size, total.max(g.moe_cache_size)),
                width,
                detail,
            ));
        }
        None => lines.push(idle_line(t, "MoE", width)),
    }

    if let Some(m) = stats.and_then(|s| s.mamba.as_ref()).filter(|m| m.total_slots > 0) {
        lines.push(meter_line(
            t,
            "GDN state",
            m.ratio(),
            width,
            format!("{} / {} slots", count(m.used_slots), count(m.total_slots)),
        ));
    }
    if let Some(s) = stats.and_then(|s| s.swa.as_ref()).filter(|s| s.total_pages > 0) {
        lines.push(meter_line(
            t,
            "SWA",
            s.ratio(),
            width,
            format!("{} / {} tok", count(s.used_tokens()), count(s.total_tokens())),
        ));
    }

    if let Some(g) = geo {
        let pools = g.pool_bytes();
        if pools.total() > 0 {
            lines.push(Line::from(""));
            let budget = g.cache_budget_bytes;
            let detail = if budget > 0 {
                format!("{} of {} budget", bytes(pools.total()), bytes(budget))
            } else {
                format!("{} allocated", bytes(pools.total()))
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", "VRAM"), t.label()),
                Span::styled(detail, t.value()),
            ]));
            lines.push(Line::from(vec![
                Span::styled(format!("{:<10}", ""), t.label()),
                Span::styled(
                    format!(
                        "KV {}  MoE {}  GDN {}  SWA {}",
                        bytes_short(pools.kv),
                        bytes_short(pools.moe),
                        bytes_short(pools.mamba),
                        bytes_short(pools.swa)
                    ),
                    t.muted(),
                ),
            ]));
        }
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled("no engine to report on", t.muted())));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn idle_line<'a>(t: &crate::ui::theme::Theme, label: &str, width: usize) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), t.label()),
        Span::styled("▕", t.muted()),
        Span::styled(crate::ui::theme::bar(0.0, width), t.muted()),
        Span::styled("▏", t.muted()),
        Span::styled("   —", t.muted()),
    ])
}

// ---------------------------------------------------------------- gpu

fn gpu_pane(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane(format!("GPU ({})", app.gpu_source), false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.gpus.is_empty() {
        let reported = app.telemetry.stats.as_ref().map(|s| s.gpus.as_slice()).unwrap_or(&[]);
        let mut lines: Vec<Line> = Vec::new();
        if reported.is_empty() {
            lines.push(Line::from(Span::styled("no NVIDIA GPU detected", t.muted())));
            if let Some(e) = app.probe_error() {
                lines.push(Line::from(Span::styled(e, t.muted())));
            }
        } else {
            lines.push(Line::from(Span::styled(
                "no local GPU readable; reporting what the engine says",
                t.muted(),
            )));
            for g in reported {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(
                            "{} ",
                            g.index.map(|i| i.to_string()).unwrap_or_else(|| "?".into())
                        ),
                        t.muted(),
                    ),
                    Span::styled(g.name.clone().unwrap_or_else(|| "GPU".into()), t.value()),
                    Span::styled(format!("   {}", bytes(g.total_bytes)), t.muted()),
                ]));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
        return;
    }

    // The engine reports the card it actually bound, which is the one that matters on a
    // multi-GPU box — flag it rather than making the reader match UUIDs by eye.
    let engine_uuid: Option<&str> =
        app.telemetry.stats.as_ref().and_then(|s| s.gpus.first()).and_then(|g| g.uuid.as_deref());

    let width = (inner.width.saturating_sub(32)).clamp(8, 28) as usize;
    let mut lines: Vec<Line> = Vec::new();
    for g in &app.gpus {
        let is_engine = engine_uuid.is_some_and(|u| u == g.uuid);
        let mut head = vec![
            Span::styled(format!("{} ", g.index), t.muted()),
            Span::styled(g.name.clone(), t.value()),
        ];
        if is_engine {
            head.push(Span::styled("  ← engine", Style::default().fg(t.good)));
        }
        lines.push(Line::from(head));
        lines.push(meter_line(
            t,
            "  VRAM",
            g.memory_ratio(),
            width,
            format!(
                "{} / {}   {} free",
                bytes(g.memory_used),
                bytes(g.memory_total),
                bytes(g.memory_free())
            ),
        ));
        if let Some(u) = g.utilization {
            lines.push(meter_line(t, "  Util", u as f64 / 100.0, width, String::new()));
        }
        let mut facts: Vec<String> = Vec::new();
        if let Some(temp) = g.temperature {
            facts.push(format!("{temp}°C"));
        }
        match (g.power_watts, g.power_limit_watts) {
            (Some(p), Some(l)) => facts.push(format!("{p:.0} / {l:.0} W")),
            (Some(p), None) => facts.push(format!("{p:.0} W")),
            _ => {}
        }
        if let Some(link) = &g.pcie_link {
            facts.push(link.clone());
        }
        if !g.uuid.is_empty() {
            facts.push(g.short_uuid());
        }
        if !facts.is_empty() {
            lines.push(Line::from(Span::styled(format!("  {}", facts.join("   ")), t.muted())));
        }
    }

    if let Some(p) = &app.bench_profile {
        lines.push(Line::from(""));
        let verdicts = app.bench_verdicts();
        if verdicts.is_empty() {
            lines.push(Line::from(Span::styled("bandwidth profile present", t.muted())));
        } else {
            // The verdict leads and the formats follow it, so the eye reads down a column
            // of answers rather than across a row of arrows.
            let width = verdicts.iter().map(|(v, _)| v.len()).max().unwrap_or(0);
            for (verdict, formats) in &verdicts {
                lines.push(Line::from(vec![
                    Span::styled(format!("{verdict:<width$}  "), t.label()),
                    Span::styled(formats.join(", "), t.muted()),
                ]));
            }
        }
        if p.cpu.physical_cores > 0 {
            lines.push(Line::from(Span::styled(
                format!(
                    "      {} cores benched, CPU {:.0} GB/s vs PCIe {:.0} GB/s",
                    p.cpu.physical_cores,
                    p.ceilings.cpu_stream_read_gbs,
                    p.ceilings.pcie_linear_h2d_gbs
                ),
                t.muted(),
            )));
        }
    } else {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "no bandwidth profile — run one from the Jobs tab (b)",
            t.muted(),
        )));
    }

    f.render_widget(Paragraph::new(lines), inner);
}

// ---------------------------------------------------------------- activity

fn activity_pane(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let block = t.pane("Activity", false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(2)])
        .split(inner);

    let mut lines: Vec<Line> = Vec::new();
    match app.telemetry.stats.as_ref() {
        Some(s) => {
            let r = &s.requests;
            lines.push(Line::from(vec![
                Span::styled(format!("{:<18}", "In flight"), t.label()),
                Span::styled(
                    r.active.to_string(),
                    Style::default()
                        .fg(if r.active > 0 { t.accent } else { t.dim })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("   {:.2} completed/s", app.completed_rate.get()), t.muted()),
            ]));
            lines.push(t.field("Completed", count(r.completed)));
            lines.push(t.field(
                "Latency",
                format!("p95 {} ms   TTFT {} ms", count(r.p95_ms), count(r.ttft_mean_ms)),
            ));
            lines.push(t.field("Prompt tokens", count(r.prompt_tokens_total)));
            lines.push(t.field("Output tokens", count(r.completion_tokens_total)));
            // Prefix reuse is the engine's own figure in principle, but it only appears
            // in a completion's usage block -- which ft-man never sees, since it polls
            // the control plane rather than proxying model traffic. This is inferred
            // from TTFT against prompt size, and is labelled so it never reads as
            // something the server reported.
            if let Some(reuse) = app.prefix_reuse() {
                lines.push(t.field_colored(
                    "Prefix reuse",
                    reuse.summary(),
                    if reuse.fraction >= 0.5 { t.good } else { t.dim },
                ));
            }
            if s.vram_bytes > 0 {
                lines.push(t.field("Engine VRAM", bytes(s.vram_bytes)));
            }
        }
        None => lines
            .push(Line::from(Span::styled("no statistics until the engine is serving", t.muted()))),
    }

    lines.push(Line::from(""));
    let jobs = app.active_jobs();
    let dls = app.active_downloads();
    if jobs > 0 || dls > 0 {
        let mut parts = Vec::new();
        if jobs > 0 {
            parts.push(format!("{jobs} job(s) running"));
        }
        if dls > 0 {
            parts.push(format!("{dls} download(s) in flight"));
        }
        lines.push(Line::from(Span::styled(parts.join(" · "), Style::default().fg(t.warn))));
    }
    lines.push(Line::from(Span::styled(
        format!("{} model(s) in the library", app.models.len()),
        t.muted(),
    )));

    f.render_widget(Paragraph::new(lines), rows[0]);

    if rows[1].height >= 2 {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled("concurrent requests", t.muted()))),
            Rect { height: 1, ..rows[1] },
        );
        render_sparkline(
            f,
            app,
            Rect { y: rows[1].y + 1, height: 1, ..rows[1] },
            app.series.active.tail(rows[1].width as usize),
            t.warn,
        );
    }
}

// ---------------------------------------------------------------- host

fn host_pane(f: &mut Frame, app: &App, area: Rect) {
    let t = &app.theme;
    let h = &app.host;
    let title = if h.hostname.is_empty() {
        "Host".to_string()
    } else {
        format!("Host — {} · {}", h.hostname, duration_secs(h.uptime_s))
    };
    let block = t.pane(title, false);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let width = (inner.width.saturating_sub(32)).clamp(8, 28) as usize;
    let mut lines = vec![
        meter_line(
            t,
            "CPU",
            (h.cpu_percent / 100.0) as f64,
            width,
            format!("{} cores   load {:.2}", h.cpu_cores, h.load_avg.0),
        ),
        meter_line(
            t,
            "RAM",
            h.memory_ratio(),
            width,
            format!("{} / {}", bytes(h.memory_used), bytes(h.memory_total)),
        ),
    ];
    if h.swap_total > 0 {
        lines.push(meter_line(
            t,
            "Swap",
            crate::util::ratio(h.swap_used, h.swap_total),
            width,
            format!("{} / {}", bytes(h.swap_used), bytes(h.swap_total)),
        ));
    }
    // Host RAM is where offloaded expert banks live, so the headroom is worth stating
    // outright rather than making the reader subtract.
    let mut tail = format!("{} free for expert banks", bytes(h.memory_free()));
    if !h.kernel.is_empty() {
        tail.push_str(&format!("   ·   kernel {}", h.kernel));
    }
    lines.push(Line::from(Span::styled(tail, t.muted())));

    f.render_widget(Paragraph::new(lines), inner);
}

impl App {
    /// The NVML failure reason, when the fallback is in use.
    fn probe_error(&self) -> Option<String> {
        (self.gpu_source == "nvidia-smi").then(|| "NVML unavailable; using nvidia-smi".to_string())
    }
}

/// Render the checkpoint's recommended sampling parameters. Reasoning models ship these
/// in `generation_config.json` and go into repetition loops without them, so it is worth
/// stating what the engine will apply to a request that specifies nothing.
pub fn format_sampling(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    let mut parts: Vec<String> = Vec::new();
    for key in ["temperature", "top_p", "top_k", "min_p", "repetition_penalty"] {
        if let Some(v) = obj.get(key).filter(|v| !v.is_null()) {
            parts.push(format!("{key} {v}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join("  "))
}
