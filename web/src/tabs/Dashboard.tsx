/**
 * Dashboard — the six panes of §5.2.
 *
 * Every derived number here (status text, context fit, reuse estimate, pool ratios,
 * completion rate) arrives ready-made in the snapshot. This file formats and lays
 * out; it does not compute.
 */

import { useCallback } from "react";
import type { ReactNode } from "react";
import type { Severity, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { Bullets, Field, Meter, Pane, Sparkline } from "../ui/primitives.tsx";
import {
  DASH,
  bytes,
  count,
  decimal,
  duration,
  fixed,
  ms,
  percent,
  text,
  tokens,
  tps,
} from "../format.ts";

export function Dashboard(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const { engine, telemetry, series, hardware } = s;
  const stats = telemetry.stats;
  const health = telemetry.health;
  const geo = telemetry.cache_status?.geometry ?? null;

  const start = useCallback(() => {
    run(api.engineStart());
  }, []);
  const stop = useCallback(() => {
    run(api.engineStop({ force: false }));
  }, []);
  const forceStop = useCallback(() => {
    run(api.engineStop({ force: true }));
  }, []);
  const smoke = useCallback(() => {
    run(api.smokeTest());
  }, []);
  const rescan = useCallback(() => {
    run(api.rescanModels());
  }, []);
  const summarize = useCallback(() => {
    run(api.summarizeUpstream());
  }, []);
  const update = useCallback(() => {
    run(api.updateFreetoken());
  }, []);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        switch (event.key) {
          case "e":
            start();
            return true;
          case "s":
            stop();
            return true;
          case "S":
            forceStop();
            return true;
          case "t":
            smoke();
            return true;
          case "r":
            rescan();
            return true;
          case "u":
            summarize();
            return true;
          case "U":
            update();
            return true;
          default:
            return false;
        }
      },
      [start, stop, forceStop, smoke, rescan, summarize, update],
    ),
  );

  const uptime = stats?.uptime_s ?? health?.uptime_s ?? null;
  const loading = health?.status === "loading";
  const fit = engine.context_fit;

  const shape = [
    fit && fit.is_truncated ? fit.summary : stats ? tokens(stats.model.ctx) : null,
    stats?.model.attn ?? null,
    stats?.model.moe ? "MoE" : null,
    // Anything past "text" is an encoder tower this engine built and is holding VRAM for.
    ...(stats?.model.input_modalities ?? []).filter((m) => m !== "text"),
  ]
    .filter((part): part is string => Boolean(part))
    .join(" · ");

  const summary = s.environment.upstream_summary;
  const gpus = hardware.gpus;
  const bench = hardware.bench_profile;

  return (
    <div className="panes three">
      {/* Column 1: Engine + Throughput */}
      <Pane
        title="Engine"
        actions={
          <>
            <button
              type="button"
              className="btn"
              onClick={start}
              // `start_blocked` is the route's own refusal predicate, so the button is
              // disabled exactly when the POST would be refused and says why in the
              // daemon's words. A disabled control that does not say why is a dead
              // end: the terminal answers the same question with a toast, which a
              // grayed button cannot.
              disabled={engine.start_blocked !== null}
              title={engine.start_blocked ?? "start the engine with the Serve configuration"}
            >
              Start <span className="dim">(e)</span>
            </button>
            <button
              type="button"
              className="btn"
              onClick={stop}
              disabled={!engine.is_live}
              title={engine.is_live ? "stop the engine" : "no engine is running"}
            >
              Stop <span className="dim">(s)</span>
            </button>
            <button
              type="button"
              className="btn danger"
              onClick={forceStop}
              disabled={!engine.is_live}
              title={engine.is_live ? "SIGKILL the engine" : "no engine is running"}
            >
              Force-stop <span className="dim">(S)</span>
            </button>
            <button
              type="button"
              className="btn"
              onClick={smoke}
              disabled={!engine.server_reachable}
              title={
                engine.server_reachable
                  ? "send one /generate request"
                  : "the server is not answering"
              }
            >
              Smoke test <span className="dim">(t)</span>
            </button>
            <button type="button" className="btn" onClick={rescan}>
              Rescan <span className="dim">(r)</span>
            </button>
            {/*
              Only offered when there is something to summarize and something to ask. The
              button is the whole feature's discoverability, so it says which of the two is
              missing rather than sitting there gray.
            */}
            {(s.environment.ft_upstream_behind ?? 0) > 0 ? (
              <button
                type="button"
                className="btn"
                onClick={summarize}
                disabled={!engine.server_reachable || s.environment.upstream_summary?.pending}
                title={
                  !engine.server_reachable
                    ? "no engine is answering; start one to ask it"
                    : "ask the loaded model what the upstream commits change"
                }
              >
                {s.environment.upstream_summary?.pending ? "Summarizing…" : "What changed?"}{" "}
                <span className="dim">(u)</span>
              </button>
            ) : null}
            {/*
              Offered only when there is something to pull. Disabled while the engine is
              live rather than hidden: the reason it cannot run now is the useful part, and
              a button that vanishes teaches nobody why.
            */}
            {(s.environment.ft_upstream_behind ?? 0) > 0 ? (
              <button
                type="button"
                className="btn"
                onClick={update}
                disabled={engine.is_live || (s.environment.ft_dirty ?? false)}
                title={
                  engine.is_live
                    ? "stop the engine first; an update rewrites the files it is running from"
                    : s.environment.ft_dirty
                      ? "the checkout has local changes; commit or discard them first"
                      : "pull the checkout and reinstall it into its venv"
                }
              >
                Update FreeToken <span className="dim">(U)</span>
              </button>
            ) : null}
            {/*
              `gpu_busy_reason` is deliberately not shown here. It explains why a GPU-heavy
              job — a conversion, a benchmark — cannot start, and none of the buttons above
              start one: every reason the Start button is disabled is already on the Start
              button, in `start_blocked`. Printed here it read as a standing instruction to
              stop the engine, in the state where the engine running is the whole point. It
              belongs on the controls it constrains, which is where the Jobs tab puts it.
            */}
          </>
        }
      >
        <Field label="Status" tone={engine.status_class}>
          <span className={`dot ${engine.status_class}`} aria-hidden="true" /> {engine.status_text}
        </Field>
        <Field label="Model">{text(engine.model ?? null)}</Field>
        {s.environment.ft_version && <Field label="FreeToken">{s.environment.ft_version}</Field>}
        <Field label="Endpoint" tone={engine.server_reachable ? "good" : "dim"} mono>
          {engine.endpoint}
        </Field>
        <Field label="Process">
          {engine.pid === null ? DASH : `pid ${engine.pid}${engine.adopted ? " (attached)" : ""}`}
        </Field>
        <Field label="Uptime">{duration(uptime)}</Field>
        <Field label="Shape">{shape === "" ? DASH : shape}</Field>
        {loading ? (
          <Meter
            label={text(health?.phase ?? null)}
            ratio={telemetry.health_load_ratio}
            figure={
              health?.progress
                ? `${bytes(health.progress.done_bytes)} / ${bytes(health.progress.total_bytes)}`
                : percent(telemetry.health_load_ratio)
            }
          />
        ) : null}
        {fit && fit.is_truncated ? (
          <div className="warn">
            KV holds {tokens(fit.usable)} of {tokens(fit.ceiling)} — plan a fix on the Serve tab
          </div>
        ) : null}
        {s.environment.ft_checkout_note ? (
          <div className="checkout">
            <Field label="Checkout" mono>
              {text(s.environment.ft_checkout_path)}
            </Field>
            {s.environment.ft_upstream_sha ? (
              <>
                {/*
                  The one line that answers "are we running the latest?": the commit the
                  tree is on, and how far that has fallen behind the remote it tracks.
                */}
                <Field
                  label="Commit"
                  tone={(s.environment.ft_upstream_behind ?? 0) > 0 ? "warn" : "good"}
                >
                  <span className="mono">{text(s.environment.ft_local_sha)}</span>{" "}
                  {(s.environment.ft_upstream_behind ?? 0) > 0
                    ? `— ${s.environment.ft_upstream_behind} behind upstream ${s.environment.ft_upstream_sha}`
                    : "— up to date with upstream"}
                </Field>
                <div className="facts">
                  {(s.environment.ft_origin_ahead ?? 0) > 0 ||
                  (s.environment.ft_origin_behind ?? 0) > 0 ? (
                    <span className="mono">
                      origin {s.environment.ft_origin_ahead ?? 0} ahead,{" "}
                      {s.environment.ft_origin_behind ?? 0} behind
                    </span>
                  ) : null}
                  {s.environment.ft_dirty ? (
                    <span className="warn">working tree has local changes</span>
                  ) : null}
                  {/*
                    Sitting on the right commit is not the same as running it: the kernels
                    are compiled, so a pull that touched their sources leaves the engine on
                    the old objects until someone rebuilds.
                  */}
                  {s.environment.ft_kernels_stale ? (
                    <span className="warn">kernels older than csrc/ — rebuild to run this commit</span>
                  ) : null}
                </div>
              </>
            ) : (
              <span className="dim">(no upstream remote, or git is not available)</span>
            )}
            {summary ? (
              <div className="summary">
                <div className="summary-head">
                  <span className="mono">{summary.range}</span>
                  <span className="dim">
                    {summary.commits} commit{summary.commits === 1 ? "" : "s"} · {summary.model}
                  </span>
                </div>
                {summary.pending ? (
                  <p className="dim">asking the model…</p>
                ) : summary.error ? (
                  <p className="bad">{summary.error}</p>
                ) : (
                  <>
                    {summary.truncated ? (
                      <p className="dim">
                        the patch was truncated to fit; the diffstat the model saw was complete
                      </p>
                    ) : null}
                    {/*
                      Written by a language model about a diff, so it is shown as what it is
                      — an account to check, not a changelog. Paragraphs and list items are
                      kept as the model wrote them rather than reflowed into one block.
                    */}
                    {summary.text?.split("\n").map((line, i) =>
                      line.trim() === "" ? null : <p key={i}>{line}</p>,
                    )}
                  </>
                )}
              </div>
            ) : null}
          </div>
        ) : null}
        <Field label="Sampling">{text(telemetry.sampling_summary)}</Field>
        {telemetry.error ? (
          <Field label="Poll" tone="bad">
            {telemetry.error}
            {telemetry.age_ms === null ? "" : ` (${duration(telemetry.age_ms / 1000)} ago)`}
          </Field>
        ) : null}
      </Pane>

      <Pane title="Throughput">
        <Field label="Decode">
          {tps(stats?.throughput.decode_tps)}{" "}
          <span className="dim">peak {decimal(series.decode_peak)}</span>
        </Field>
        <Sparkline values={series.decode_tps} label="decode tokens per second" tone="good" />
        <Field label="Prefill">{tps(stats?.throughput.prefill_tps)}</Field>
        <Sparkline values={series.prefill_tps} label="prefill tokens per second" tone="dim" />
      </Pane>

      {/* Column 2: Cache pools + GPU */}
      <Pane title="Cache pools">
        <Meter
          label="KV"
          ratio={telemetry.kv_ratio}
          figure={
            telemetry.kv_total_tokens
              ? `${tokens(telemetry.kv_used_tokens)} / ${tokens(telemetry.kv_total_tokens)}`
              : "idle"
          }
        />
        <Meter
          label="MoE"
          ratio={
            geo && telemetry.total_experts
              ? geo.moe_cache_size / telemetry.total_experts
              : null
          }
          figure={
            geo && telemetry.total_experts
              ? `${count(geo.moe_cache_size)} / ${count(telemetry.total_experts)}`
              : "idle"
          }
        />
        <Meter
          label="GDN state"
          ratio={telemetry.mamba_ratio}
          figure={
            stats?.mamba
              ? `${count(stats.mamba.used_slots)} / ${count(stats.mamba.total_slots)}`
              : "idle"
          }
        />
        <Meter
          label="SWA"
          ratio={telemetry.swa_ratio}
          figure={
            telemetry.swa_total_tokens
              ? `${tokens(telemetry.swa_used_tokens)} / ${tokens(telemetry.swa_total_tokens)}`
              : "idle"
          }
        />
        <Field label="VRAM">
          {bytes(telemetry.pool_bytes?.total)}
          {geo && geo.cache_budget_bytes > 0 ? ` of ${bytes(geo.cache_budget_bytes)}` : ""}
        </Field>
        <div className="facts">
          <span>KV {bytes(telemetry.pool_bytes?.kv)}</span>
          <span>MoE {bytes(telemetry.pool_bytes?.moe)}</span>
          <span>GDN {bytes(telemetry.pool_bytes?.mamba)}</span>
          <span>SWA {bytes(telemetry.pool_bytes?.swa)}</span>
        </div>
      </Pane>

      <Pane title={`GPU (${hardware.gpu_source})`}>
        {gpus.length > 0 ? (
          gpus.map((gpu) => (
            <div key={gpu.uuid}>
              <Field label={`${gpu.index}`}>
                {gpu.name}
                {gpu.uuid === hardware.engine_gpu_uuid ? (
                  <span className="accent"> ← engine</span>
                ) : null}
              </Field>
              <Meter
                label="VRAM"
                ratio={gpu.memory_ratio}
                figure={`${percent(gpu.memory_ratio)}`}
                tone={gpu.memory_ratio > 0.95 ? "warn" : undefined}
              />
              <div className="facts">
                <span>
                  {bytes(gpu.memory_used)} used · {bytes(gpu.memory_total)} total ·{" "}
                  {bytes(gpu.memory_free)} free
                </span>
              </div>
              <Meter
                label="Util"
                ratio={gpu.utilization === null ? null : gpu.utilization / 100}
                figure={gpu.utilization === null ? DASH : `${gpu.utilization}%`}
              />
              <div className="facts">
                <span>{gpu.temperature === null ? DASH : `${gpu.temperature}°C`}</span>
                <span>
                  {fixed(gpu.power_watts, 0)} / {fixed(gpu.power_limit_watts, 0)} W
                </span>
                <span>{text(gpu.pcie_link)}</span>
                <span className="mono">{gpu.short_uuid}</span>
              </div>
            </div>
          ))
        ) : hardware.reported_gpus.length > 0 ? (
          <>
            <p className="dim">no local GPU readable; reporting what the engine says</p>
            {hardware.reported_gpus.map((card, i) => (
              <Field key={card.uuid ?? i} label={`${card.index ?? i}`}>
                {text(card.name)} · {bytes(card.total_bytes)}
              </Field>
            ))}
          </>
        ) : (
          <p className="dim">no NVIDIA GPU detected</p>
        )}
        {bench === null ? (
          <Field label="Bench" tone="dim">
            no bandwidth profile — run one from the Jobs tab (b)
          </Field>
        ) : (
          <>
            {/*
              One row per verdict rather than one per format. The profile gives every
              quantization format the same answer on most machines, so `fmt→verdict`
              repeated five times says one thing five times and hides the only thing worth
              scanning for: a format that disagrees. Grouped, the usual case is a single
              row and a disagreement is a second one that cannot be missed.
            */}
            <Field label="Bench">
              {hardware.bench_verdicts.length === 0 ? (
                <span className="dim">measured, but no per-format verdict</span>
              ) : (
                <span className="verdicts">
                  {hardware.bench_verdicts.map((v) => (
                    <span key={v.verdict} className="verdict">
                      <span className="verdict-name">{v.verdict}</span>
                      <span className="verdict-formats mono">{v.formats.join(", ")}</span>
                    </span>
                  ))}
                </span>
              )}
            </Field>
            <div className="facts">
              <span>{bench.cpu.threads_used} of {bench.cpu.physical_cores} cores benched</span>
              <span>CPU {fixed(bench.ceilings.cpu_stream_read_gbs)} GB/s</span>
              <span>PCIe {fixed(bench.ceilings.pcie_linear_h2d_gbs)} GB/s h2d</span>
            </div>
          </>
        )}
      </Pane>

      {/* Column 3: Host + Activity */}
      <Pane
        title={`Host — ${hardware.host.hostname}`}
        note={`up ${duration(hardware.host.uptime_s)}`}
      >
        <Meter
          label="CPU"
          ratio={hardware.host.cpu_percent / 100}
          figure={`${fixed(hardware.host.cpu_percent)}%`}
        />
        <div className="facts">
          <span>{hardware.host.cpu_cores} threads · {hardware.host.physical_cores} cores</span>
          <span>load {fixed(hardware.host.load_avg[0], 2)}</span>
        </div>
        <Meter
          label="RAM"
          ratio={hardware.host.memory_ratio}
          figure={`${bytes(hardware.host.memory_used)} / ${bytes(hardware.host.memory_total)}`}
        />
        {hardware.host.swap_total > 0 ? (
          <Meter
            label="Swap"
            ratio={hardware.host.swap_used / hardware.host.swap_total}
            figure={`${bytes(hardware.host.swap_used)} / ${bytes(hardware.host.swap_total)}`}
            tone={hardware.host.swap_used > 0 ? "warn" : undefined}
          />
        ) : null}
        <div className="facts">
          <span>{bytes(hardware.host.memory_free)} free for expert banks</span>
          <span className="mono">{hardware.host.kernel}</span>
        </div>
        {s.environment.ft_error ? (
          <Bullets items={[{ level: "bad" as Severity, text: s.environment.ft_error }]} />
        ) : null}
      </Pane>

      <Pane title="Activity">
        <Field label="In flight">
          {count(stats?.requests.active)}{" "}
          <span className="dim">{decimal(engine.completed_rate, 2)} completed/s</span>
        </Field>
        <Field label="Completed">{count(stats?.requests.completed)}</Field>
        <Field label="Latency">
          p95 {ms(stats?.requests.p95_ms)} · TTFT {ms(stats?.requests.ttft_mean_ms)}
        </Field>
        <Field label="Prompt tokens">{count(stats?.requests.prompt_tokens_total)}</Field>
        <Field label="Output tokens">{count(stats?.requests.completion_tokens_total)}</Field>
        {engine.prefix_reuse ? (
          <Field label="Prefix reuse">{engine.prefix_reuse.summary}</Field>
        ) : null}
        {stats && stats.vram_bytes > 0 ? (
          <Field label="Engine VRAM">{bytes(stats.vram_bytes)}</Field>
        ) : null}
        <Field label="Work">
          {engine.active_jobs} job{engine.active_jobs === 1 ? "" : "s"} ·{" "}
          {engine.active_downloads} download{engine.active_downloads === 1 ? "" : "s"}
        </Field>
        <Field label="Library">
          {count(s.models.items.length)} checkpoint{s.models.items.length === 1 ? "" : "s"}
        </Field>
        <Sparkline values={series.active} label="concurrent requests" tone="dim" />
        <div className="dim">concurrent requests</div>
      </Pane>
    </div>
  );
}
