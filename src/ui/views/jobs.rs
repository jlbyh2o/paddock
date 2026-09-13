//! The Jobs view: FTW conversions, bandwidth benchmarks, and Hub downloads.
//!
//! All three are long-running, all three want a progress bar and a log tail, and all
//! three are things a person starts and then walks away from — so they share one screen
//! with a list on top and the selected job's live output underneath.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use crate::ft::proc::JobStatus;
use crate::hub::DownloadStatus;
use crate::ui::app::App;
use crate::ui::theme::bar;
use crate::util::{bytes, duration_secs, eta, rate, truncate};

/// A row in the unified list: either a CLI job or a download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Job(usize),
    Download(usize),
}

/// Everything currently listed, jobs first, newest at the bottom of each section.
pub fn rows(app: &App) -> Vec<Row> {
    let mut out: Vec<Row> = (0..app.jobs.len()).map(Row::Job).collect();
    out.extend((0..app.downloads.len()).map(Row::Download));
    out
}

pub fn render(f: &mut Frame, app: &mut App, area: Rect) {
    let split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(area);

    list(f, app, split[0]);
    output(f, app, split[1]);
}

fn list(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let items = rows(app);
    let running = app.active_jobs() + app.active_downloads();
    let title = if running > 0 { format!("Jobs ({} active)", running) } else { "Jobs".to_string() };
    let block = t.pane(title, !app.jobs_view.in_output);
    let inner = block.inner(area);
    f.render_widget(block, area);

    if items.is_empty() {
        let msg = match &app.convert_checking {
            Some(path) => format!(
                "Checking that FreeToken can read {}…\n\nThis resolves the checkpoint \
                 through FreeToken's own config so a conversion that cannot read the \
                 experts fails in seconds rather than minutes.",
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            ),
            None => "Nothing running.\n\n\
                 c  convert the selected model to FTW (from the Models tab)\n\
                 b  benchmark CPU vs PCIe bandwidth and calibrate the MoE backend\n\
                 d  download a repo (from the Hub tab)"
                .to_string(),
        };
        f.render_widget(Paragraph::new(msg).style(t.muted()).wrap(Wrap { trim: false }), inner);
        return;
    }

    // Two rows per entry: headline, then the progress bar.
    let capacity = ((inner.height as usize) / 2).max(1);
    app.jobs_view.sel.clamp(items.len());
    let range = app.jobs_view.sel.window(items.len(), capacity);
    let selected = app.jobs_view.sel.index;
    let width = inner.width as usize;

    let mut lines: Vec<Line> = Vec::new();
    for i in range {
        let is_sel = i == selected;
        match items[i] {
            Row::Job(j) => lines.extend(job_rows(app, j, is_sel, width)),
            Row::Download(d) => lines.extend(download_rows(app, d, is_sel, width)),
        }
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn job_rows<'a>(app: &App, idx: usize, is_sel: bool, width: usize) -> Vec<Line<'a>> {
    let t = &app.theme;
    let job = &app.jobs[idx];
    let label = job.status.label();
    let color = match &job.status {
        JobStatus::Running => t.warn,
        JobStatus::Done => t.good,
        JobStatus::Failed(_) => t.bad,
        JobStatus::Canceled => t.dim,
    };

    let title_w = width.saturating_sub(30);
    let head = Line::from(vec![
        Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
        Span::styled(
            format!("{:<9}", job.kind.label()),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<title_w$}", truncate(&job.title, title_w)),
            if is_sel { t.selected() } else { t.text() },
        ),
        Span::styled(format!("{label:>9}  "), Style::default().fg(color)),
        Span::styled(duration_secs(job.elapsed().as_secs()), t.muted()),
    ]);

    let bar_w = width.saturating_sub(38).clamp(8, 40);
    let detail = match (&job.status, job.progress.ratio()) {
        (JobStatus::Failed(why), _) => {
            let reason = job.failure_reason().unwrap_or_else(|| why.clone());
            Line::from(vec![
                Span::raw("           "),
                Span::styled(
                    truncate(&reason, width.saturating_sub(12)),
                    Style::default().fg(t.bad),
                ),
            ])
        }
        (_, Some(r)) => Line::from(vec![
            Span::raw("           "),
            Span::styled("▕", t.muted()),
            Span::styled(bar(r, bar_w), Style::default().fg(color)),
            Span::styled("▏ ", t.muted()),
            Span::styled(format!("{:>3.0}%  ", r * 100.0), Style::default().fg(color)),
            Span::styled(progress_detail(job), t.muted()),
        ]),
        _ => Line::from(vec![
            Span::raw("           "),
            Span::styled(
                if job.progress.phase.is_empty() {
                    "starting…".to_string()
                } else {
                    format!("{} — {}", job.progress.phase, progress_detail(job))
                },
                t.muted(),
            ),
        ]),
    };

    vec![head, detail]
}

pub fn progress_detail(job: &crate::ft::Job) -> String {
    let p = &job.progress;
    if p.bytes {
        // The converter reports no total for its dense phase, so there is no bar and no
        // ETA. A live rate is what distinguishes "working" from "wedged", and its
        // absence is why a running conversion can read as frozen.
        let speed = job.rate.get();
        let moving = job.is_running() && speed > 1.0;
        if p.total > 0 {
            let base = format!("{} / {}  ({})", bytes(p.done), bytes(p.total), p.phase);
            if moving {
                format!("{base}  {}", rate(speed))
            } else {
                base
            }
        } else if p.done > 0 {
            if moving {
                format!("{} written  {}  ({})", bytes(p.done), rate(speed), p.phase)
            } else {
                format!("{} written  ({})", bytes(p.done), p.phase)
            }
        } else {
            p.phase.clone()
        }
    } else if p.total > 0 {
        format!("step {} of {}  {}", p.done, p.total, p.phase)
    } else {
        p.phase.clone()
    }
}

fn download_rows<'a>(app: &App, idx: usize, is_sel: bool, width: usize) -> Vec<Line<'a>> {
    let t = &app.theme;
    let d = &app.downloads[idx];
    let label = d.status.label();
    let color = match &d.status {
        DownloadStatus::Running => t.warn,
        DownloadStatus::Done => t.good,
        DownloadStatus::Failed(_) => t.bad,
        DownloadStatus::Canceled => t.dim,
    };

    let title_w = width.saturating_sub(34);
    let head = Line::from(vec![
        Span::styled(if is_sel { "▌" } else { " " }, Style::default().fg(t.accent)),
        Span::styled(
            format!("{:<9}", "download"),
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<title_w$}", truncate(&d.repo, title_w)),
            if is_sel { t.selected() } else { t.text() },
        ),
        Span::styled(format!("{label:>13}  "), Style::default().fg(color)),
        Span::styled(duration_secs(d.elapsed().as_secs()), t.muted()),
    ]);

    let bar_w = width.saturating_sub(38).clamp(8, 40);
    let r = d.ratio();
    let speed = d.rate.get();
    let detail_text = match &d.status {
        DownloadStatus::Failed(why) => truncate(why, width.saturating_sub(12)),
        DownloadStatus::Running => format!(
            "{} / {}   {}   ETA {}   {} of {} files",
            bytes(d.done()),
            bytes(d.total_bytes),
            rate(speed),
            eta(d.total_bytes.saturating_sub(d.done()), speed),
            d.files_done,
            d.file_count
        ),
        _ => format!("{} / {}", bytes(d.done()), bytes(d.total_bytes)),
    };

    let detail = Line::from(vec![
        Span::raw("           "),
        Span::styled("▕", t.muted()),
        Span::styled(bar(r, bar_w), Style::default().fg(color)),
        Span::styled("▏ ", t.muted()),
        Span::styled(format!("{:>3.0}%  ", r * 100.0), Style::default().fg(color)),
        Span::styled(detail_text, t.muted()),
    ]);

    vec![head, detail]
}

fn output(f: &mut Frame, app: &mut App, area: Rect) {
    let t = &app.theme;
    let items = rows(app);
    let selected = items.get(app.jobs_view.sel.index).copied();

    let title = match selected {
        Some(Row::Job(i)) => format!("Output — {}", app.jobs[i].title),
        Some(Row::Download(i)) => format!("Detail — {}", app.downloads[i].repo),
        None => "Output".to_string(),
    };
    let block = t.pane(title, app.jobs_view.in_output);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = match selected {
        Some(Row::Job(i)) => {
            let job = &app.jobs[i];
            let mut out = vec![
                Line::from(Span::styled(job.command_line.clone(), t.muted())),
                Line::from(Span::styled(format!("log: {}", job.log_path.display()), t.muted())),
            ];
            if let Some(p) = &job.output_path {
                out.push(Line::from(Span::styled(
                    format!("profile: {}", p.display()),
                    Style::default().fg(t.good),
                )));
            }
            out.push(Line::from(""));

            let snapshot = job.log.snapshot();
            let height = inner.height.saturating_sub(out.len() as u16) as usize;
            let skip = snapshot.len().saturating_sub(height + app.jobs_view.output_scroll);
            let take = height.min(snapshot.len().saturating_sub(skip));
            out.extend(snapshot.iter().skip(skip).take(take).map(|l| {
                Line::from(Span::styled(
                    l.text.clone(),
                    if l.err { Style::default().fg(t.warn) } else { t.text() },
                ))
            }));
            out
        }
        Some(Row::Download(i)) => {
            let d = &app.downloads[i];
            let mut out = vec![
                t.field("Repo", d.repo.clone()),
                t.field("Revision", d.revision.clone()),
                t.field("Target", d.target.display().to_string()),
                t.field("Files", format!("{} of {}", d.files_done, d.file_count)),
                t.field("Transferred", format!("{} of {}", bytes(d.done()), bytes(d.total_bytes))),
            ];
            if d.is_running() {
                out.push(t.field("Rate", rate(d.rate.get())));
                out.push(t.field("Current file", truncate(&d.current, inner.width as usize)));
            }
            if let DownloadStatus::Failed(why) = &d.status {
                out.push(Line::from(""));
                out.push(Line::from(Span::styled(why.clone(), Style::default().fg(t.bad))));
            }
            out
        }
        None => bench_profile_lines(app, inner.width as usize),
    };

    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

/// With nothing selected, the output pane shows the machine's bandwidth profile.
///
/// This is the measurement that decides `--moe-strategy auto` between `offload` and
/// `hybrid`, and it is otherwise buried in a JSON file under `~/.cache/freetoken`. Since
/// it is also what the `b` key on this screen produces, it belongs here.
fn bench_profile_lines<'a>(app: &App, width: usize) -> Vec<Line<'a>> {
    let t = &app.theme;
    let Some(p) = app.bench_profile.as_ref() else {
        return vec![
            Line::from(Span::styled("No bandwidth profile on this machine.", t.muted())),
            Line::from(""),
            Line::from(Span::styled(
                "Press b to run `ft bench bw`. It measures host-RAM versus PCIe bandwidth with \
                 the real MoE kernels and writes a per-GPU profile, which is what lets \
                 --moe-strategy auto choose hybrid over offload.",
                t.muted(),
            )),
        ];
    };

    let mut lines = vec![Line::from(vec![
        Span::styled(
            "Bandwidth profile",
            Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("   v{}", p.version), t.muted()),
    ])];

    if let Some(name) = &p.gpu.name {
        let idx = p.gpu.index.map(|i| format!("[{i}] ")).unwrap_or_default();
        lines.push(t.field("GPU", format!("{idx}{name}")));
    }
    if let Some(uuid) = &p.gpu.uuid {
        lines.push(t.field("UUID", truncate(uuid, width.saturating_sub(20))));
    }
    if let Some(host) = &p.host {
        lines.push(t.field("Benched on", host.clone()));
    }
    if let Some(ts) = &p.timestamp {
        lines.push(t.field("When", ts.clone()));
    }
    lines.push(t.field(
        "CPU",
        format!("{} physical cores, {} threads used", p.cpu.physical_cores, p.cpu.threads_used),
    ));
    lines.push(t.field(
        "Ceilings",
        format!(
            "RAM read {:.1}  ·  PCIe H2D {:.1}  ·  D2H {:.1} GB/s",
            p.ceilings.cpu_stream_read_gbs,
            p.ceilings.pcie_linear_h2d_gbs,
            p.ceilings.pcie_linear_d2h_gbs
        ),
    ));
    lines.push(t.field(
        "Threshold",
        format!("recommend hybrid when CPU beats PCIe by {:.1}x", p.threshold),
    ));

    if !p.dtype_kernels.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::styled(format!("{:<14}", "FORMAT"), t.header()),
            Span::styled(format!("{:>11}", "CPU GB/s"), t.header()),
            Span::styled(format!("{:>11}", "PCIe GB/s"), t.header()),
            Span::styled(format!("{:>8}", "RATIO"), t.header()),
            Span::styled(format!("  {:<9}", "VERDICT"), t.header()),
            Span::styled("ISA", t.header()),
        ]));
        for (fmt, k) in &p.dtype_kernels {
            let verdict = k.recommended.as_deref().unwrap_or("—");
            let color = match verdict {
                "hybrid" => t.good,
                "offload" => t.accent,
                _ => t.dim,
            };
            lines.push(Line::from(vec![
                Span::styled(format!("{fmt:<14}"), t.text()),
                Span::styled(format!("{:>11}", gbs(k.cpu_moe_gbs)), t.text()),
                Span::styled(format!("{:>11}", gbs(k.pcie_gather_gbs)), t.text()),
                Span::styled(
                    format!(
                        "{:>8}",
                        k.ratio.map(|r| format!("{r:.2}x")).unwrap_or_else(|| "—".into())
                    ),
                    t.muted(),
                ),
                Span::styled(format!("  {verdict:<9}"), Style::default().fg(color)),
                Span::styled(k.cpu_moe_isa.clone().unwrap_or_default(), t.muted()),
            ]));
            if let (Some(c), Some(p)) = (k.cpu_moe_overlap_gbs, k.pcie_gather_overlap_gbs) {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!(
                            "contended: CPU {c:.1} vs PCIe {p:.1} GB/s — the split                              --moe-hybrid-max-fetch auto uses"
                        ),
                        t.muted(),
                    ),
                ]));
            }
            if let Some(note) = &k.note {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(truncate(note, width.saturating_sub(4)), t.muted()),
                ]));
            }
        }
    }

    lines
}

fn gbs(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.1}")).unwrap_or_else(|| "—".into())
}
