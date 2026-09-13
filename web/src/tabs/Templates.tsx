/**
 * Templates — §5.5.
 *
 * The TUI's "apply this template" means "to the model selected on the Models tab",
 * a cross-tab cursor a browser has no equivalent of — so §1.5 makes `model_path` an
 * explicit parameter and this tab carries its own model picker. Nothing here
 * depends on what any other tab is showing.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import type { FormEvent, ReactNode } from "react";
import type { Severity, Sibling, Snapshot, StoredTemplateEntry } from "../api/types.ts";
import { api } from "../api/client.ts";
import { reportError, run } from "../api/store.ts";
import { useTabKeys } from "../ui/keys.ts";
import { useSelection } from "../ui/useSelection.ts";
import { Empty, Field, Pane } from "../ui/primitives.tsx";
import { SearchField, useFilterField } from "../ui/SearchField.tsx";
import { DASH, basename, bytes, count, shortSha, text, timestamp } from "../format.ts";

const OUTCOME_TONE: Record<string, Severity> = { ok: "good", warn: "warn", fail: "bad" };

export function Templates(props: { snapshot: Snapshot }): ReactNode {
  const s = props.snapshot;
  const t = s.templates;

  // The repo is being composed, not filtering anything on screen, so Escape gives the
  // keyboard back rather than emptying it.
  const repo = useFilterField(t.repo, { clearOnEscape: false });
  /** The checkpoint the reader picked, if they picked one and it is still there. */
  const [chosenPath, setChosenPath] = useState<string | null>(null);
  const [pane, setPane] = useState<"stored" | "remote">("stored");
  const [preview, setPreview] = useState<{ name: string; textBody: string; truncated: boolean } | null>(
    null,
  );

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

  /**
   * The apply target, reconciled against the library on every snapshot.
   *
   * A rescan, a delete or a conversion can remove the checkpoint that was chosen, and
   * a path that is no longer in the library is not a target an action may be aimed at
   * — so the choice falls back to the first checkpoint, and to nothing at all when the
   * library is empty.
   */
  const model = useMemo(() => {
    const items = s.models.items;
    const chosen = chosenPath === null ? null : items.find((m) => m.path === chosenPath);
    return chosen ?? items[0] ?? null;
  }, [chosenPath, s.models.items]);
  const modelPath = model?.path ?? null;

  const listRepo = useCallback(
    (event?: FormEvent) => {
      event?.preventDefault();
      if (repo.needle === "") return;
      run(api.templatesListRepo({ repo: repo.needle }));
    },
    [repo],
  );

  const fetchSelected = useCallback(() => {
    const file = remote.item;
    const source = t.remote_repo ?? repo.needle;
    if (!file || source.trim() === "") return;
    run(
      api.templatesFetch({
        repo: source,
        path: file.path,
        ...(t.remote_revision === null ? {} : { revision: t.remote_revision }),
      }),
    );
  }, [remote.item, t.remote_repo, t.remote_revision, repo]);

  useTabKeys(
    useCallback(
      (event: KeyboardEvent) => {
        if (repo.handleKey(event)) return true;
        if (event.key === "Escape") return false;
        const active = pane === "stored" ? stored : remote;
        if (active.handleKey(event)) return true;
        switch (event.key) {
          case "r":
            repo.focus();
            return true;
          case "Enter":
            if (pane === "remote") fetchSelected();
            else listRepo();
            return true;
          case "f":
            fetchSelected();
            return true;
          case "a":
            if (selectedName && modelPath) {
              run(api.templatesApply({ template: selectedName, model_path: modelPath }));
            }
            return true;
          case "u":
            if (modelPath) run(api.templatesRevert({ model_path: modelPath }));
            return true;
          case "v":
            if (selectedName && modelPath) {
              run(api.templatesVerify({ template: selectedName, model_path: modelPath }));
            }
            return true;
          case "D":
            if (selectedName) run(api.templatesDelete({ name: selectedName }));
            return true;
          default:
            return false;
        }
      },
      [pane, repo, stored, remote, fetchSelected, listRepo, selectedName, modelPath],
    ),
  );

  const preflight =
    t.preflight && selectedName && t.preflight.template === selectedName ? t.preflight : null;

  return (
    <div className="stack">
      <Pane title="Browse a repo" note={t.loading ? "(loading…)" : undefined}>
        <form className="inline-form" onSubmit={listRepo}>
          <SearchField
            field={repo}
            list="template-sources"
            placeholder="org/templates  (r)"
            label="Template repository"
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
          title={`In ${basename(t.remote_repo ?? repo.value)}`}
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
                  if (selectedName && modelPath) {
                    run(api.templatesVerify({ template: selectedName, model_path: modelPath }));
                  }
                }}
                disabled={!selectedName || !modelPath}
              >
                Verify <span className="dim">(v)</span>
              </button>
              <button
                type="button"
                className="btn danger"
                onClick={() => {
                  if (selectedName) run(api.templatesDelete({ name: selectedName }));
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
                  if (selectedName && modelPath) {
                    run(api.templatesApply({ template: selectedName, model_path: modelPath }));
                  }
                }}
                disabled={!selectedName || !modelPath}
              >
                Apply <span className="dim">(a)</span>
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => {
                  if (modelPath) run(api.templatesRevert({ model_path: modelPath }));
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
              value={modelPath ?? ""}
              disabled={s.models.items.length === 0}
              onChange={(e) => setChosenPath(e.target.value === "" ? null : e.target.value)}
              style={{ flex: "1 1 220px" }}
            >
              {modelPath === null ? <option value="">— no checkpoint in the library —</option> : null}
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
