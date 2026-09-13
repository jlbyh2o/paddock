/**
 * Hub — §5.4.
 *
 * Search, a repo's compatibility verdict *before* anything is downloaded, a
 * quantization to pick, a file list to adjust, and the download itself. The free
 * space figure is always shown with the path it was measured at, because
 * `hub::disk_free_at` walks up to an existing ancestor and can land on a different
 * filesystem than the one the download will use.
 */

import { useCallback, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import type { CompatVerdict, RepoFile, RepoSummary, Severity, Snapshot } from "../api/types.ts";
import { api } from "../api/client.ts";
import { run } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Empty, Field, Pane } from "../ui/primitives.tsx";
import { DASH, bytes, count, dateOnly, shortSha } from "../format.ts";

const VERDICT_TONE: Record<CompatVerdict, Severity> = {
  supported: "good",
  caution: "warn",
  unsupported: "bad",
  unknown: "dim",
};

const NOTE_TONE: Record<string, Severity> = {
  info: "dim",
  caution: "warn",
  blocker: "bad",
};

export function Hub(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const hub = s.hub;
  const [query, setQuery] = useState(hub.query);
  const [pane, setPane] = useState<"results" | "files">("results");
  const searchRef = useRef<HTMLInputElement>(null);

  const results = useSelection(hub.results, useCallback((r: RepoSummary) => r.id, []));
  const files = useSelection(hub.files, useCallback((f: RepoFile) => f.path, []));

  const openRepo = useCallback((repoId: string) => {
    run(api.hubOpen(repoId));
  }, []);

  const download = useCallback(() => {
    const repoId = hub.info?.id ?? results.id;
    if (!repoId) return;
    run(api.hubDownload({ repo_id: repoId, revision: hub.revision }));
  }, [hub.info?.id, hub.revision, results.id]);

  const submitSearch = (event: FormEvent) => {
    event.preventDefault();
    if (query.trim() === "") return;
    run(api.hubSearch(query.trim()));
  };

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (event.key === "Escape") {
          searchRef.current?.blur();
          return false;
        }
        const active = pane === "results" ? results : files;
        if (active.handleKey(event)) return true;
        switch (event.key) {
          case "/":
            searchRef.current?.focus();
            searchRef.current?.select();
            return true;
          case "Enter":
            if (pane === "results" && results.item) openRepo(results.item.id);
            return true;
          case " ":
            if (pane === "files" && files.item) {
              run(api.hubToggleFile(files.item.path));
              return true;
            }
            return false;
          case "a":
            run(api.hubSelectFiles("all"));
            return true;
          case "n":
            run(api.hubSelectFiles("none"));
            return true;
          case "d":
            download();
            return true;
          case "i":
            if (!hub.hf_cli) run(api.hubInstallCli());
            return true;
          default:
            return false;
        }
      },
      [pane, results, files, openRepo, download, hub.hf_cli],
    ),
  );

  const compat = hub.compat;
  const compatTitle = hub.checking_compat
    ? "Compatibility — checking…"
    : hub.compat_error
      ? "Compatibility — could not check"
      : compat
        ? `Compatibility — ${compat.verdict_label}`
        : "Download to";

  const variants = hub.layout?.variants.filter((v) => v.role === "weights") ?? [];

  return (
    <div className="stack">
      <div className="panes wide">
        <Pane
          title="Search"
          note={[
            hub.searching ? "(searching…)" : null,
            s.environment.hub_token_present ? "(authenticated)" : null,
          ]
            .filter(Boolean)
            .join(" ")}
          bodyClassName="flush"
          actions={
            <form className="inline-form" onSubmit={submitSearch} style={{ width: "100%" }}>
              <input
                ref={searchRef}
                type="search"
                value={query}
                placeholder="search Hugging Face  (/)"
                onChange={(e) => setQuery(e.target.value)}
                aria-label="Search Hugging Face"
              />
              <button type="submit" className="btn primary">
                Search
              </button>
            </form>
          }
        >
          {hub.results.length === 0 ? (
            <Empty>
              <p>
                Nothing listed yet. FreeToken's known-good checkpoints are a good start:
                Qwen3.6-35B-A3B, Qwen3.8-Flash-Next, DeepSeek-V3.2.
              </p>
              <p>
                {s.environment.hub_token_present
                  ? `Authenticated with ${s.environment.hub_token_source ?? "a token"}.`
                  : "No Hugging Face token configured, so gated repos will not list."}
              </p>
            </Empty>
          ) : (
            <ul className="rows scroll h-420">
              {hub.results.map((repo) => (
                <li
                  key={repo.id}
                  className={`row ${repo.id === results.id ? "selected" : ""}`}
                  onClick={() => {
                    setPane("results");
                    results.select(repo.id);
                  }}
                  onDoubleClick={() => openRepo(repo.id)}
                >
                  <div className="row-main">
                    <span className="grow mono">{repo.id}</span>
                    {repo.is_gated ? <span className="warn">gated</span> : null}
                    {repo.private ? <span className="dim">private</span> : null}
                  </div>
                  <div className="row-sub">
                    {count(repo.downloads)} downloads · {count(repo.likes)} likes ·{" "}
                    {dateOnly(repo.last_modified)} · {repo.interesting_tags.join(" ")}
                  </div>
                </li>
              ))}
            </ul>
          )}
        </Pane>

        <div className="stack">
          {hub.layout?.is_multi ? (
            <Pane
              title={
                hub.variant && !hub.custom_selection
                  ? `Quantization — ${hub.variant}`
                  : `Quantization — ${variants.length} available, none chosen`
              }
              bodyClassName="flush"
            >
              <ul className="rows">
                {variants.map((variant) => (
                  <li
                    key={variant.label}
                    className="row"
                    onClick={() => run(api.hubVariant(variant.label))}
                  >
                    <div className="row-main">
                      <span>
                        {!hub.custom_selection && hub.variant === variant.label ? "●" : "○"}
                      </span>
                      <span className="grow">{variant.label}</span>
                      <span className="dim nowrap">
                        {bytes(variant.bytes)} · {variant.file_count} pts
                      </span>
                    </div>
                  </li>
                ))}
              </ul>
            </Pane>
          ) : null}

          <Pane title={compatTitle}>
            {compat ? (
              <>
                <Field label="Verdict" tone={VERDICT_TONE[compat.verdict]}>
                  {compat.verdict_label}
                </Field>
                <Field label="Summary">{compat.summary}</Field>
                {compat.notes.length === 0 ? (
                  <p className="dim">nothing known stands in the way</p>
                ) : (
                  <ul className="bullets">
                    {compat.notes.slice(0, 3).map((note, i) => (
                      <li key={i} className={NOTE_TONE[note.level] ?? "dim"}>
                        <span>{note.text}</span>
                      </li>
                    ))}
                  </ul>
                )}
              </>
            ) : hub.compat_error ? (
              <>
                <Field label="Error" tone="bad">
                  {hub.compat_error}
                </Field>
                <p className="dim">
                  The verdict needs the repo's config.json; without it nothing can be said
                  before the download.
                </p>
              </>
            ) : (
              <p className="dim">Open a repo to check it against this machine.</p>
            )}

            {hub.info ? (
              <Field label="Repo" mono>
                {hub.info.id} @ {hub.revision} ({shortSha(hub.info.sha)})
                {hub.info.is_gated ? <span className="warn"> gated</span> : null}
              </Field>
            ) : null}
            <Field label="Target" mono>
              {hub.target}
            </Field>
            {hub.hf_cli ? null : hub.hf_installing ? (
              <Field label="hf CLI" tone="warn">
                Installing…
              </Field>
            ) : (
              <Field label="hf CLI" tone="bad">
                the hf CLI is not installed — downloads are impossible until it is
              </Field>
            )}
            {hub.selected_count > 0 ? (
              <Field label="Download">
                {bytes(hub.selected_bytes)} to download ·{" "}
                {hub.disk_free
                  ? `${bytes(hub.disk_free.free_bytes)} free on ${hub.disk_free.measured_path}`
                  : `${DASH} free`}
              </Field>
            ) : null}
          </Pane>
        </div>
      </div>

      <Pane
        title="Files"
        note={
          hub.loading_info
            ? "(loading…)"
            : `${count(hub.selected_count)} of ${count(hub.files.length)} selected, ${bytes(hub.selected_bytes)}`
        }
        bodyClassName="flush"
        actions={
          <>
            <button type="button" className="btn" onClick={() => run(api.hubSelectFiles("all"))}>
              Select all <span className="dim">(a)</span>
            </button>
            <button type="button" className="btn" onClick={() => run(api.hubSelectFiles("none"))}>
              None <span className="dim">(n)</span>
            </button>
            <button
              type="button"
              className="btn primary"
              onClick={download}
              disabled={!hub.hf_cli || hub.selected_count === 0}
            >
              Download <span className="dim">(d)</span>
            </button>
            {hub.hf_cli ? null : (
              <button
                type="button"
                className="btn"
                onClick={() => run(api.hubInstallCli())}
                disabled={hub.hf_installing}
              >
                Install the hf CLI <span className="dim">(i)</span>
              </button>
            )}
          </>
        }
      >
        {hub.files.length === 0 ? (
          <Empty>
            <p>Open a repo from the results to list its files.</p>
          </Empty>
        ) : (
          <div className="scroll h-340">
            {hub.files.map((file) => (
              <label
                key={file.path}
                className={`checkline ${file.path === files.id ? "selected" : ""}`}
                onClick={() => {
                  setPane("files");
                  files.select(file.path);
                }}
              >
                <input
                  type="checkbox"
                  checked={file.wanted}
                  onChange={(e) => run(api.hubToggleFile(file.path, e.target.checked))}
                />
                <span className="truncate">{file.path}</span>
                <span className="size">{bytes(file.size)}</span>
              </label>
            ))}
          </div>
        )}
      </Pane>
    </div>
  );
}
