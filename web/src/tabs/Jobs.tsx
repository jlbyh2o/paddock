/**
 * Jobs — §5.8.
 *
 * Jobs and downloads are two lists with two id counters that collide (§7.6), so a
 * row's identity here is the pair of kind and id, and cancel goes to whichever of
 * the two routes matches. Output comes from `GET /api/jobs/{id}/output`, which
 * already has the progress protocol filtered out so the browser sees what the TUI's
 * pane shows.
 */

import { useCallback, useMemo } from "react";
import type { ReactNode } from "react";
import type { Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run, useJobOutput } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Bar, Empty, Field, Pane } from "../ui/primitives.tsx";
import { bytes, duration, eta, fixed, gbs, rate, text, timestamp } from "../format.ts";

type Row =
  | { kind: "job"; id: number; key: string }
  | { kind: "download"; id: number; key: string };

export function Jobs(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const jobs = s.jobs;

  const rows = useMemo<Row[]>(
    () => [
      ...jobs.items.map((j): Row => ({ kind: "job", id: j.id, key: `job:${j.id}` })),
      ...jobs.downloads.map((d): Row => ({ kind: "download", id: d.id, key: `dl:${d.id}` })),
    ],
    [jobs.items, jobs.downloads],
  );

  const selection = useSelection(rows, useCallback((r: Row) => r.key, []));
  const selected = selection.item;

  const job = selected?.kind === "job" ? jobs.items.find((j) => j.id === selected.id) ?? null : null;
  const download =
    selected?.kind === "download" ? jobs.downloads.find((d) => d.id === selected.id) ?? null : null;

  const output = useJobOutput(
    job ? job.id : null,
    true,
    job?.is_running ?? false,
    job?.output_bytes ?? 0,
  );

  const cancel = useCallback(() => {
    if (!selected) return;
    if (selected.kind === "job") run(api.cancelJob(selected.id));
    else run(api.cancelDownload(selected.id));
  }, [selected]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (selection.handleKey(event)) return true;
        switch (event.key) {
          case "b":
            run(api.runBench());
            return true;
          case "x":
            cancel();
            return true;
          case "X":
            run(api.clearFinishedJobs());
            return true;
          default:
            return false;
        }
      },
      [selection, cancel],
    ),
  );

  const active = s.engine.active_jobs + s.engine.active_downloads;
  const bench = s.hardware.bench_profile;

  return (
    <div className="panes wide">
      <Pane
        title="Jobs and downloads"
        note={active > 0 ? `${active} active` : undefined}
        bodyClassName="flush"
        actions={
          <>
            <button
              type="button"
              className="btn"
              onClick={() => run(api.runBench())}
              disabled={s.engine.gpu_busy_reason !== null}
              title={s.engine.gpu_busy_reason ?? "measure this machine's bandwidth profile"}
            >
              Run bench <span className="dim">(b)</span>
            </button>
            <button
              type="button"
              className="btn danger"
              onClick={cancel}
              disabled={!((job?.is_running ?? false) || (download?.is_running ?? false))}
            >
              Cancel <span className="dim">(x)</span>
            </button>
            <button type="button" className="btn" onClick={() => run(api.clearFinishedJobs())}>
              Clear finished <span className="dim">(X)</span>
            </button>
          </>
        }
      >
        {rows.length === 0 ? (
          <Empty>
            {jobs.convert_checking ? (
              <p>Checking that FreeToken can read {jobs.convert_checking}…</p>
            ) : (
              <>
                <p>Nothing running. Work starts in three places:</p>
                <ul>
                  <li>Models — convert a checkpoint to FTW</li>
                  <li>Hub — download a repo</li>
                  <li>here — run a bandwidth benchmark with b</li>
                </ul>
              </>
            )}
          </Empty>
        ) : (
          <ul className="rows scroll h-560">
            {jobs.items.map((entry) => (
              <li
                key={`job:${entry.id}`}
                className={`row ${selection.id === `job:${entry.id}` ? "selected" : ""}`}
                onClick={() => selection.select(`job:${entry.id}`)}
              >
                <div className="row-main">
                  <span className="tag">{entry.kind_label}</span>
                  <span className="grow">{entry.title}</span>
                  <span
                    className={
                      entry.status.kind === "failed"
                        ? "bad"
                        : entry.status.kind === "done"
                          ? "good"
                          : "dim"
                    }
                  >
                    {entry.status_label}
                  </span>
                  <span className="dim nowrap">{duration(entry.elapsed_s)}</span>
                </div>
                {entry.progress_ratio !== null ? (
                  <Bar
                    ratio={entry.progress_ratio}
                    tone={entry.status.kind === "failed" ? "bad" : undefined}
                  />
                ) : null}
                <div className={`row-sub ${entry.failure_reason ? "bad" : ""}`}>
                  {entry.failure_reason ?? entry.progress_detail}
                </div>
              </li>
            ))}
            {jobs.downloads.map((entry) => (
              <li
                key={`dl:${entry.id}`}
                className={`row ${selection.id === `dl:${entry.id}` ? "selected" : ""}`}
                onClick={() => selection.select(`dl:${entry.id}`)}
              >
                <div className="row-main">
                  <span className="tag">download</span>
                  <span className="grow mono">{entry.repo}</span>
                  <span
                    className={
                      entry.status.kind === "failed"
                        ? "bad"
                        : entry.status.kind === "done"
                          ? "good"
                          : "dim"
                    }
                  >
                    {entry.status_label}
                  </span>
                  <span className="dim nowrap">{duration(entry.elapsed_s)}</span>
                </div>
                <Bar ratio={entry.ratio} />
                <div className="row-sub">
                  {bytes(entry.done_bytes)} / {bytes(entry.total_bytes)} · {rate(entry.rate_bps)} ·
                  ETA {eta(entry.eta_s)} · {entry.files_done} of {entry.file_count} files
                </div>
              </li>
            ))}
          </ul>
        )}
      </Pane>

      <Pane
        title={job ? "Output" : download ? "Download" : "Bandwidth profile"}
        note={output.restarted && job ? "log rotated — restarted" : undefined}
      >
        {job ? (
          <>
            <Field label="Command" mono>
              {job.command_line}
            </Field>
            <Field label="Log" mono>
              {job.log_path}
            </Field>
            {job.output_path ? (
              <Field label="Wrote" mono>
                {job.output_path}
              </Field>
            ) : null}
            <Field label="Started">{timestamp(job.started_at)}</Field>
            {job.finished_at ? <Field label="Finished">{timestamp(job.finished_at)}</Field> : null}
            {job.failure_reason ? (
              <Field label="Failed" tone="bad">
                {job.failure_reason}
              </Field>
            ) : null}
            <pre className="loglist">{output.lines.join("\n")}</pre>
          </>
        ) : download ? (
          <>
            <Field label="Repo" mono>
              {download.repo}
            </Field>
            <Field label="Revision" mono>
              {download.revision}
            </Field>
            <Field label="Target" mono>
              {download.target}
            </Field>
            <Field label="Files">
              {download.files_done} of {download.file_count}
            </Field>
            <Field label="Transferred">
              {bytes(download.done_bytes)} of {bytes(download.total_bytes)}
            </Field>
            {download.is_running ? (
              <>
                <Field label="Rate">
                  {rate(download.rate_bps)} · ETA {eta(download.eta_s)}
                </Field>
                <Field label="Current" mono>
                  {download.current}
                </Field>
              </>
            ) : null}
            {download.failure_reason ? (
              <Field label="Failed" tone="bad">
                {download.failure_reason}
              </Field>
            ) : null}
          </>
        ) : bench ? (
          <>
            <Field label="Version">{bench.version}</Field>
            <Field label="GPU">{text(bench.gpu.name)}</Field>
            <Field label="UUID" mono>
              {text(bench.gpu.uuid)}
            </Field>
            <Field label="Host">{text(bench.host)}</Field>
            <Field label="Measured">{timestamp(bench.timestamp)}</Field>
            <Field label="CPU">
              {bench.cpu.physical_cores} cores · {bench.cpu.threads_used} threads used
            </Field>
            <Field label="CPU read">{gbs(bench.ceilings.cpu_stream_read_gbs)}</Field>
            <Field label="PCIe h2d">{gbs(bench.ceilings.pcie_linear_h2d_gbs)}</Field>
            <Field label="PCIe d2h">{gbs(bench.ceilings.pcie_linear_d2h_gbs)}</Field>
            <Field label="Hybrid at">ratio ≥ {fixed(bench.threshold, 2)}</Field>
            <div className="table-wrap">
              <table className="grid">
                <thead>
                  <tr>
                    <th>format</th>
                    <th className="r">CPU GB/s</th>
                    <th className="r">PCIe GB/s</th>
                    <th className="r">ratio</th>
                    <th>verdict</th>
                    <th>ISA</th>
                  </tr>
                </thead>
                <tbody>
                  {Object.entries(bench.dtype_kernels).map(([format, kernel]) => (
                    <tr key={format}>
                      <td className="mono">{format}</td>
                      <td className="r">{fixed(kernel.cpu_moe_gbs)}</td>
                      <td className="r">{fixed(kernel.pcie_gather_gbs)}</td>
                      <td className="r">{fixed(kernel.ratio, 2)}</td>
                      <td>{text(kernel.recommended)}</td>
                      <td className="mono">{text(kernel.cpu_moe_isa)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {Object.entries(bench.dtype_kernels).map(([format, kernel]) =>
              kernel.cpu_moe_overlap_gbs !== null || kernel.pcie_gather_overlap_gbs !== null ? (
                <div className="dim" key={`${format}-overlap`}>
                  {format} contended: CPU {fixed(kernel.cpu_moe_overlap_gbs)} GB/s · PCIe{" "}
                  {fixed(kernel.pcie_gather_overlap_gbs)} GB/s
                </div>
              ) : null,
            )}
            {Object.entries(bench.dtype_kernels).map(([format, kernel]) =>
              kernel.note ? (
                <div className="warn" key={`${format}-note`}>
                  {format}: {kernel.note}
                </div>
            ) : null,
            )}
            {s.hardware.bench_profile_path ? (
              <Field label="Read from" mono>
                {s.hardware.bench_profile_path}
              </Field>
            ) : null}
          </>
        ) : (
          <Empty>
            <p>No bandwidth profile on this machine. Run one with b.</p>
            <p className="dim">
              Without it, <span className="mono">--moe-strategy auto</span> is decided from
              defaults rather than from this machine's measurements.
            </p>
          </Empty>
        )}
      </Pane>
    </div>
  );
}
