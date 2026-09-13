/**
 * Templates — §5.5.
 *
 * The TUI's "apply this template" means "to the model selected on the Models tab",
 * a cross-tab cursor a browser has no equivalent of — so §1.5 makes `model_path` an
 * explicit parameter and this tab carries its own model picker. Nothing here
 * depends on what any other tab is showing.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import type { Severity, Sibling, Snapshot, StoredTemplateEntry } from "../api/types.ts";
import { api } from "../api/client.ts";
import { reportError, run } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Empty, Field, Pane } from "../ui/primitives.tsx";
import { DASH, basename, bytes, count, shortSha, text, timestamp } from "../format.ts";

const OUTCOME_TONE: Record<string, Severity> = { ok: "good", warn: "warn", fail: "bad" };

export function Templates(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const t = s.templates;

  const [repo, setRepo] = useState(t.repo);
  const [modelPath, setModelPath] = useState<string>(() => s.models.items[0]?.path ?? "");
  const [pane, setPane] = useState<"stored" | "remote">("stored");
  const [preview, setPreview] = useState<{ name: string; textBody: string; truncated: boolean } | null>(
    null,
  );
  const repoRef = useRef<HTMLInputElement>(null);

  const stored = useSelection(t.stored, useCallback((e: StoredTemplateEntry) => e.name, []));
  const remote = useSelection(t.remote, useCallback((f: Sibling) => f.path, []));

  const selectedName = stored.item?.name ?? null;

  useEffect(() => {
    if (!selectedName) {
      setPreview(null);
      return;
    }
    let live = true;
    api
      .templatePreview(selectedName)
      .then((page) => {
        if (live) setPreview({ name: page.name, textBody: page.text, truncated: page.truncated });
      })
      .catch((error: unknown) => {
        if (live) {
          setPreview(null);
          reportError(error);
        }
      });
    return () => {
      live = false;
    };
  }, [selectedName]);

  const model = s.models.items.find((m) => m.path === modelPath) ?? null;

  const listRepo = useCallback(
    (event?: FormEvent) => {
      event?.preventDefault();
      if (repo.trim() === "") return;
      run(api.templatesListRepo(repo.trim()));
    },
    [repo],
  );

  const fetchSelected = useCallback(() => {
    const file = remote.item;
    const source = t.remote_repo ?? repo;
    if (!file || source.trim() === "") return;
    run(api.templatesFetch(source, file.path, t.remote_revision ?? undefined));
  }, [remote.item, t.remote_repo, t.remote_revision, repo]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (event.key === "Escape") {
          repoRef.current?.blur();
          return false;
        }
        const active = pane === "stored" ? stored : remote;
        if (active.handleKey(event)) return true;
        switch (event.key) {
          case "r":
          case "/":
            repoRef.current?.focus();
            repoRef.current?.select();
            return true;
          case "Enter":
            if (pane === "remote") fetchSelected();
            else listRepo();
            return true;
          case "f":
            fetchSelected();
            return true;
          case "a":
            if (selectedName && modelPath) run(api.templatesApply(selectedName, modelPath));
            return true;
          case "u":
            if (modelPath) run(api.templatesRevert(modelPath));
            return true;
          case "v":
            if (selectedName && modelPath) run(api.templatesVerify(selectedName, modelPath));
            return true;
          case "D":
            if (selectedName) run(api.templatesDelete(selectedName));
            return true;
          default:
            return false;
        }
      },
      [pane, stored, remote, fetchSelected, listRepo, selectedName, modelPath],
    ),
  );

  const preflight =
    t.preflight && selectedName && t.preflight.template === selectedName ? t.preflight : null;

  return (
    <div className="stack">
      <Pane title="Browse a repo" note={t.loading ? "(loading…)" : undefined}>
        <form className="inline-form" onSubmit={listRepo}>
          <input
            ref={repoRef}
            type="text"
            value={repo}
            list="template-sources"
            placeholder="org/templates  (r)"
            onChange={(e) => setRepo(e.target.value)}
            aria-label="Template repository"
          />
          <datalist id="template-sources">
            {t.sources.map((src) => (
              <option key={src} value={src} />
            ))}
          </datalist>
          <button type="submit" className="btn primary">
            List templates <span className="dim">(Enter)</span>
          </button>
          <span className="dim">
            {t.preflight_enabled ? "an apply runs a render check first" : "render checks are off"}
          </span>
        </form>
      </Pane>

      <div className="panes">
        <Pane title="Stored" note={count(t.stored.length)} bodyClassName="flush">
          {t.stored.length === 0 ? (
            <Empty>
              <p>No templates stored yet. Fetch one from a repo above.</p>
            </Empty>
          ) : (
            <ul className="rows scroll h-260">
              {t.stored.map((entry) => (
                <li
                  key={entry.name}
                  className={`row ${entry.name === stored.id ? "selected" : ""}`}
                  onClick={() => {
                    setPane("stored");
                    stored.select(entry.name);
                  }}
                >
                  <div className="row-main">
                    <span className="grow">{entry.name}</span>
                    <span className="dim nowrap">{bytes(entry.size)}</span>
                  </div>
                  <div className="row-sub">{entry.subtitle}</div>
                </li>
              ))}
            </ul>
          )}
        </Pane>

        <Pane
          title={`In ${basename(t.remote_repo ?? repo)}`}
          note={count(t.remote.length)}
          bodyClassName="flush"
          actions={
            <button
              type="button"
              className="btn"
              onClick={fetchSelected}
              disabled={!remote.item}
            >
              Fetch <span className="dim">(f)</span>
            </button>
          }
        >
          {t.remote.length === 0 ? (
            <Empty>
              <p>List a repo to see the .jinja files it holds.</p>
            </Empty>
          ) : (
            <ul className="rows scroll h-260">
              {t.remote.map((file, i) => (
                <li
                  key={file.path}
                  className={`row ${file.path === remote.id ? "selected" : ""}`}
                  onClick={() => {
                    setPane("remote");
                    remote.select(file.path);
                  }}
                  onDoubleClick={fetchSelected}
                >
                  <div className="row-main">
                    <span className="good">
                      {t.remote_stored_names[i] &&
                      t.stored.some((e) => e.name === t.remote_stored_names[i])
                        ? "✓"
                        : " "}
                    </span>
                    <span className="grow mono">{file.path}</span>
                    <span className="dim nowrap">{bytes(file.size)}</span>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </Pane>

        <Pane
          title="Preview"
          note={stored.item?.name}
          actions={
            <>
              <button
                type="button"
                className="btn"
                onClick={() => {
                  if (selectedName && modelPath) run(api.templatesVerify(selectedName, modelPath));
                }}
                disabled={!selectedName || !modelPath}
              >
                Verify <span className="dim">(v)</span>
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => {
                  if (selectedName) run(api.templatesDelete(selectedName));
                }}
                disabled={!selectedName}
              >
                Delete <span className="dim">(D)</span>
              </button>
            </>
          }
        >
          {!stored.item ? (
            <Empty>
              <p>Select a stored template.</p>
            </Empty>
          ) : (
            <>
              <Field label="Version">{text(stored.item.meta.version)}</Field>
              <Field label="Source">{text(stored.item.meta.source)}</Field>
              <Field label="Repo path" mono>
                {text(stored.item.meta.repo_path)}
              </Field>
              <Field label="Revision" mono>
                {shortSha(stored.item.meta.revision)}
              </Field>
              <Field label="Fetched">{timestamp(stored.item.meta.fetched_at)}</Field>
              <Field
                label="Render check"
                tone={
                  t.checking
                    ? "dim"
                    : preflight
                      ? (OUTCOME_TONE[preflight.outcome.kind] ?? "dim")
                      : "dim"
                }
              >
                {t.checking
                  ? "running…"
                  : preflight
                    ? preflight.outcome.detail
                    : "not checked against a model yet"}
              </Field>
              <pre className="loglist" style={{ maxHeight: 220 }}>
                {preview?.textBody ?? t.preview?.text ?? ""}
              </pre>
              {(preview?.truncated ?? t.preview?.truncated) ? (
                <p className="dim">… truncated at 8 KiB</p>
              ) : null}
            </>
          )}
        </Pane>

        <Pane
          title="Apply to"
          actions={
            <>
              <button
                type="button"
                className="btn primary"
                onClick={() => {
                  if (selectedName && modelPath) run(api.templatesApply(selectedName, modelPath));
                }}
                disabled={!selectedName || !modelPath}
              >
                Apply <span className="dim">(a)</span>
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => {
                  if (modelPath) run(api.templatesRevert(modelPath));
                }}
                disabled={!modelPath}
              >
                Restore built-in <span className="dim">(u)</span>
              </button>
            </>
          }
        >
          <div className="inline-form">
            <label htmlFor="apply-model">Model</label>
            <select
              id="apply-model"
              value={modelPath}
              onChange={(e) => setModelPath(e.target.value)}
              style={{ flex: "1 1 220px" }}
            >
              <option value="">— choose a checkpoint —</option>
              {s.models.items.map((m) => (
                <option key={m.path} value={m.path}>
                  {m.name}
                </option>
              ))}
            </select>
          </div>
          {!model ? (
            <Empty>
              <p>
                Applying writes <span className="mono">chat_template.jinja</span> into the
                checkpoint directory, and into its FTW build when one exists.
              </p>
            </Empty>
          ) : (
            <>
              <Field label="Currently">{model.template_status.label}</Field>
              {model.template_targets.length === 0 ? (
                <Field label="Writes into" tone="warn">
                  {DASH} this checkpoint has no directory to write into
                </Field>
              ) : (
                model.template_targets.map((target) => (
                  <Field key={target} label="Writes into" mono>
                    {target}
                  </Field>
                ))
              )}
              {model.template_targets.length > 1 ? (
                <p className="dim">
                  Both the checkpoint and its FTW build are written, so the override survives
                  whichever one is served.
                </p>
              ) : null}
              {preflight &&
              preflight.outcome.kind === "fail" &&
              model.template_status.kind === "overridden" ? (
                <p className="warn">
                  The last render check failed. Restore the checkpoint's own template before
                  serving it.
                </p>
              ) : null}
            </>
          )}
        </Pane>
      </div>
    </div>
  );
}
